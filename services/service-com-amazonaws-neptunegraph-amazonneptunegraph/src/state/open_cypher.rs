// SPDX-License-Identifier: CC0-1.0

//! The READING half of the openCypher data plane: how a graph's STORED nodes and edges
//! are read by the query engine, and how a result set and a refused query are reported.
//!
//! `ExecuteQuery` is the one operation that runs a caller-supplied query, and it runs it
//! for real: a text whose clauses are all READING ones is lexed and parsed by the
//! `cypher-parser` crate's openCypher parser and evaluated by that crate's executor,
//! which reads the graph ONLY through its [`GraphProvider`] trait. [`StoredGraph`] is
//! this service's implementation of that trait over [`super::GraphData`], so a query
//! answers from the same nodes and edges every other reading of the graph (a summary, an
//! export, a restore) is computed from — there is no second copy of a graph's contents
//! and nothing is synthesised to look computed.
//!
//! A text carrying an UPDATING clause, or binding its data through `$name` parameters, is
//! read and evaluated by the engine that has a production for it
//! (`super::open_cypher_updates`) — over the same stored nodes and edges, and reported
//! through the same failures and the same payload envelope ([`rows_payload`]). Which
//! engine a text reaches is decided there, from a real parse of it.
//!
//! What the engine decides and what this module decides are kept apart:
//!
//! * whether the query is WELL FORMED, and what it evaluates to, is the engine's —
//!   including the position a malformed query is refused at and the constructs that
//!   could have stood there, both of which this module only words
//!   ([`syntax_refusal`], [`expected_alternatives`]);
//! * how a node, an edge property or a result row is SPELLED on the wire is this
//!   service's, because that is the service's own protocol ([`run_query`]).
//!
//! The engine's value domain is openCypher's scalars (`null`, boolean, integer, string),
//! nodes, lists and maps, which is narrower than the JSON a property can be written
//! with: a FRACTIONAL number has no integer form, so it is read as the text of its JSON
//! number rather than silently truncated to an integer. Nothing else about a stored
//! property changes on the way in or out.
//!
//! ## What a query may do
//!
//! The evaluation happens IN THIS PROCESS over the data it is handed and nothing else:
//! the engine opens no socket, reads no file and starts nothing. This engine's grammar
//! accepts only `MATCH`, `OPTIONAL MATCH`, `UNWIND` and `WITH` before a `RETURN`, so a
//! query naming a clause NEITHER of this data plane's engines runs is reported as the
//! clause this endpoint does not run ([`unsupported_clause`]) rather than as a text that
//! does not parse, and a function neither implements is refused by the engine itself.
//!
//! What a caller's query does choose is how much work it asks for, so every bound on
//! that is stated here, on this side of the engine, and each is taken BEFORE the work it
//! caps: the text one request may have read ([`super::MAX_QUERY_STRING_CHARS`]), how
//! deep that text may nest ([`MAX_QUERY_NESTING_DEPTH`]) and how many tokens it may
//! carry at all ([`MAX_QUERY_TOKENS`]) — the pair that bounds how far one parse of it
//! descends — the rows a query's own literals may make ([`literal_rows`]), the candidate
//! elements one evaluation may be handed ([`MAX_QUERY_WORK_UNITS`]) and the span it may
//! spend being handed them ([`query_run_timeout`]). [`run_query`] takes no lock and reads
//! no store, so the operation runs it with the store's guard already given up, off the
//! thread that serves requests and under one of a bounded number of permits
//! ([`MAX_CONCURRENT_QUERIES`]).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ::cypher_parser::{CypherValue, GraphProvider, ResultSet};

use super::GraphData;

/// Why one evaluation stopped before it had answered.
///
/// The engine reads the graph only through [`StoredGraph`], so what it is handed is
/// where an evaluation is STOPPED: once one of these is recorded the provider answers
/// nothing more, the engine unwinds, and the operation reports the limit that was met
/// rather than the short result the engine was left holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The evaluation asked for more of the graph than one query may read.
    Exhausted,
    /// The span the query was given ran out while it was still asking.
    TimedOut,
    /// The query it belongs to was cancelled while it was still asking.
    Cancelled,
}

/// The handle one running evaluation is stopped through when the query it belongs to is
/// cancelled.
///
/// A cancel arrives on ANOTHER request than the one that is running the query
/// (`CancelQuery`), and the two share nothing but the record the store holds, so the
/// record carries this handle: accepting a cancel signals it, and the evaluation reads it
/// at every question it asks the graph ([`Budget::charge`]) and stops. It is the only
/// thing the two requests share, so no store is locked and no evaluation is waited on to
/// stop one.
///
/// Cloning a handle shares the one flag: every clone speaks for the same evaluation.
#[derive(Debug, Clone, Default)]
pub struct QueryCancellation(Arc<AtomicBool>);

impl QueryCancellation {
    /// A handle to an evaluation nothing has cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop the evaluation this handle speaks for, at the next question it asks.
    pub fn signal(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether the evaluation this handle speaks for has been cancelled.
    #[must_use]
    pub fn is_signalled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What one evaluation may spend, and why it stopped if it did.
///
/// Charged in CANDIDATE ELEMENTS: every node the provider hands the engine costs one,
/// and every question the engine asks costs one — which is what makes the ceiling bound
/// the rows the engine can build, since the executor produces a row per candidate it was
/// handed and asks the provider again for every row it carries forward. The deadline is
/// read once per question, so a query whose cost diverges stops ASKING at the deadline
/// rather than running on past it.
#[derive(Debug)]
struct Budget {
    /// How many candidate elements this evaluation may still be handed.
    left: Cell<usize>,
    /// The instant after which it may be handed none.
    deadline: Instant,
    /// Why it stopped, once it has.
    stopped: Cell<Option<Stopped>>,
    /// The cancel of the query this evaluation belongs to, once one has been accepted.
    cancelled: QueryCancellation,
}

impl Budget {
    /// Charge `units` of the budget, answering whether the provider may answer at all.
    ///
    /// Checked BEFORE the work it pays for, so nothing past the ceiling is computed, and
    /// the first charge a question cannot afford is what records the stop: from then on
    /// every charge fails, which is how the engine is brought to a halt through the only
    /// surface it has.
    fn charge(&self, units: usize) -> bool {
        if self.stopped.get().is_some() {
            return false;
        }
        // A cancel the service has accepted is read here, beside the deadline, because
        // this is the one place the evaluation can be stopped from: a cancelled query
        // stops ASKING rather than running to the end and answering a caller that is no
        // longer owed an answer.
        if self.cancelled.is_signalled() {
            self.stopped.set(Some(Stopped::Cancelled));
            return false;
        }
        if Instant::now() >= self.deadline {
            self.stopped.set(Some(Stopped::TimedOut));
            return false;
        }
        let left = self.left.get();
        if units > left {
            self.stopped.set(Some(Stopped::Exhausted));
            return false;
        }
        self.left.set(left - units);
        true
    }
}

/// One graph's stored data, as the query engine reads it, under the budget one
/// evaluation may spend.
///
/// Built once per query from the graph's own [`super::GraphData`] and dropped with the
/// call: the engine asks about labels, adjacency and properties many times per query, so
/// the two adjacency indexes below are built in ONE pass over the edges rather than
/// walked per question — a query over a graph of n nodes and m edges costs one pass over
/// each, whatever the pattern it traverses.
#[derive(Debug)]
pub struct StoredGraph<'data> {
    /// The data every answer is read from.
    data: &'data GraphData,
    /// Every node id, in the store's own order: a node is addressed by its INDEX here,
    /// which is the cheap `Copy` handle the engine's `NodeId` asks for.
    nodes: Vec<&'data str>,
    /// For each relationship type, the targets of every node that leaves an edge of it.
    outgoing: BTreeMap<&'data str, BTreeMap<usize, Vec<usize>>>,
    /// The same adjacency read backwards: for each relationship type, the sources of
    /// every node an edge of it enters.
    incoming: BTreeMap<&'data str, BTreeMap<usize, Vec<usize>>>,
    /// What this evaluation may still spend, and why it stopped if it did.
    budget: Budget,
}

impl<'data> StoredGraph<'data> {
    /// Index `data` for one query that may be handed `units` candidate elements and must
    /// stop asking at `deadline`.
    #[must_use]
    pub fn over_within(data: &'data GraphData, units: usize, deadline: Instant) -> Self {
        let nodes: Vec<&str> = data.nodes.keys().map(String::as_str).collect();
        // Where each node id sits in `nodes`, so an edge's endpoints resolve to handles
        // in one lookup rather than a scan over the nodes per edge.
        let index_of: BTreeMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        let mut outgoing: BTreeMap<&str, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
        let mut incoming: BTreeMap<&str, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
        for edge in data.edges.values() {
            // An edge the store holds always joins two nodes it holds
            // (`GraphData::insert_edge` refuses any other), so an endpoint that does not
            // resolve is nothing to answer about rather than a case to invent.
            let (Some(from), Some(to)) = (
                index_of.get(edge.from_node.as_str()),
                index_of.get(edge.to_node.as_str()),
            ) else {
                continue;
            };
            outgoing
                .entry(edge.label.as_str())
                .or_default()
                .entry(*from)
                .or_default()
                .push(*to);
            incoming
                .entry(edge.label.as_str())
                .or_default()
                .entry(*to)
                .or_default()
                .push(*from);
        }
        Self {
            data,
            nodes,
            outgoing,
            incoming,
            budget: Budget {
                left: Cell::new(units),
                deadline,
                stopped: Cell::new(None),
                cancelled: QueryCancellation::new(),
            },
        }
    }

    /// The same reading of the graph, stopped as soon as `cancellation` is signalled:
    /// what the operation that admitted the query hands the evaluation so an accepted
    /// `CancelQuery` really ends the work.
    #[must_use]
    pub fn cancelled_by(mut self, cancellation: QueryCancellation) -> Self {
        self.budget.cancelled = cancellation;
        self
    }

    /// Why the evaluation over this graph stopped, if it did.
    #[must_use]
    pub fn stopped(&self) -> Option<Stopped> {
        self.budget.stopped.get()
    }

    /// The candidates `answers` once the budget has paid for them, or none at all when
    /// it cannot: a question the budget cannot afford is answered with nothing, which is
    /// what stops the engine asking more.
    fn afford(&self, answers: Vec<usize>) -> Vec<usize> {
        if self.budget.charge(answers.len()) {
            answers
        } else {
            Vec::new()
        }
    }

    /// The stored node one handle names, or `None` for a handle this graph does not hold.
    fn node(&self, node: usize) -> Option<&'data super::GraphNode> {
        self.nodes.get(node).and_then(|id| self.data.nodes.get(*id))
    }

    /// The neighbours of `node` along `rel_type` in one direction's index.
    fn neighbours(
        index: &BTreeMap<&'data str, BTreeMap<usize, Vec<usize>>>,
        node: usize,
        rel_type: &str,
    ) -> Vec<usize> {
        index
            .get(rel_type)
            .and_then(|by_node| by_node.get(&node))
            .cloned()
            .unwrap_or_default()
    }
}

impl GraphProvider for StoredGraph<'_> {
    type NodeId = usize;

    fn scan(&self, labels: &[String]) -> Vec<Self::NodeId> {
        // The question itself is charged BEFORE the scan runs, so a budget that is spent
        // pays for no further pass over the nodes.
        if !self.budget.charge(1) {
            return Vec::new();
        }
        let selected: Vec<usize> = (0..self.nodes.len())
            .filter(|node| {
                // "An empty slice matches every node", and a node carries several
                // labels, so a disjunction selects it when ANY of its labels is named.
                labels.is_empty()
                    || self.node(*node).is_some_and(|stored| {
                        labels.iter().any(|label| stored.labels.contains(label))
                    })
            })
            .collect();
        self.afford(selected)
    }

    fn matches_label(&self, node: Self::NodeId, label: &str) -> bool {
        if !self.budget.charge(1) {
            return false;
        }
        self.node(node)
            .is_some_and(|stored| stored.labels.contains(label))
    }

    fn relationship_types(&self) -> Vec<String> {
        if !self.budget.charge(1) {
            return Vec::new();
        }
        self.data
            .edges
            .values()
            .map(|edge| edge.label.clone())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect()
    }

    fn expand(&self, node: Self::NodeId, rel_type: &str) -> Vec<Self::NodeId> {
        if !self.budget.charge(1) {
            return Vec::new();
        }
        self.afford(Self::neighbours(&self.outgoing, node, rel_type))
    }

    fn rel_sources(&self, rel_type: &str) -> Vec<Self::NodeId> {
        if !self.budget.charge(1) {
            return Vec::new();
        }
        let sources = self
            .outgoing
            .get(rel_type)
            .map(|by_node| by_node.keys().copied().collect())
            .unwrap_or_default();
        self.afford(sources)
    }

    fn expand_in(&self, node: Self::NodeId, rel_type: &str) -> Option<Vec<Self::NodeId>> {
        // The store holds both ends of every edge, so the reverse step is answered from
        // the index built with the forward one rather than left to the engine to rebuild
        // the whole graph's reverse adjacency for.
        if !self.budget.charge(1) {
            return Some(Vec::new());
        }
        Some(self.afford(Self::neighbours(&self.incoming, node, rel_type)))
    }

    fn property(&self, node: Self::NodeId, prop: &str) -> CypherValue {
        if !self.budget.charge(1) {
            return CypherValue::Null;
        }
        let Some(stored) = self
            .node(node)
            .and_then(|stored| stored.properties.get(prop))
        else {
            return CypherValue::Null;
        };
        // A value the engine can EXPAND costs what it can expand TO: `UNWIND` over a
        // stored list makes one row per element, and the engine builds those rows
        // without asking the provider anything further, so every element handed over is
        // charged HERE — before the value is — and a query chaining expansions pays for
        // each row it carries forward. A value the budget cannot afford is not handed
        // over at all, which stops the evaluation exactly as a question it cannot afford
        // does.
        if !self.budget.charge(value_elements(stored)) {
            return CypherValue::Null;
        }
        engine_value(stored)
    }

    fn node_id(&self, node: Self::NodeId) -> String {
        if !self.budget.charge(1) {
            return String::new();
        }
        self.nodes.get(node).map_or_else(String::new, |id| (*id).to_string())
    }

    fn label(&self, node: Self::NodeId) -> String {
        // openCypher lets a node carry several labels and the engine asks for ONE; the
        // store holds them as an ordered set, so the one reported is the same on every
        // call rather than whichever a hash order yields.
        if !self.budget.charge(1) {
            return String::new();
        }
        self.node(node)
            .and_then(|stored| stored.labels.iter().next().cloned())
            .unwrap_or_default()
    }

    fn name(&self, node: Self::NodeId) -> String {
        // The display name of a node that carries one: the `name` property is what
        // openCypher examples and this service's own bulk-load formats name an element
        // by. A node without one is displayed by the id it is addressed by.
        if !self.budget.charge(1) {
            return String::new();
        }
        self.node(node)
            .and_then(|stored| stored.properties.get(NAME_PROPERTY))
            .and_then(|value| value.as_str().map(ToString::to_string))
            .unwrap_or_else(|| self.node_id(node))
    }
}

/// The property a node's display name is read from when it carries one.
const NAME_PROPERTY: &str = "name";

/// The reading clause a query that reads NOTHING of the graph is parsed behind.
///
/// It binds exactly one row and no element of the graph: `UNWIND` over a one-item list
/// is the engine's own way of making rows out of a literal, so the projection behind it
/// is evaluated once, which is what openCypher answers a query with no reading clause
/// with.
const ONE_ROW_CLAUSE: &str = "UNWIND [0] AS unitrow";

/// Parse `query` as openCypher reads it, or report where it could not be read.
///
/// openCypher lets a query PROJECT without reading the graph at all (`RETURN 1 AS one`),
/// which answers one row of the projected expressions. The engine's grammar requires a
/// reading clause in front of a projection (`MATCH`, `OPTIONAL MATCH`, `UNWIND` or
/// `WITH`), so such a query is read behind the clause that binds one row and nothing of
/// the graph ([`ONE_ROW_CLAUSE`]) — the caller's own projection is still what the engine
/// parses and evaluates, and the row count is the one openCypher states.
///
/// A text neither reading accepts is refused with the error of the query AS THE CALLER
/// SENT IT, so the position a refusal states is a position in the caller's own text and
/// never in a text this service composed.
pub fn parse_query(query: &str) -> Result<::cypher_parser::Query, ::cypher_parser::CypherError> {
    match ::cypher_parser::parse(query) {
        Ok(parsed) => Ok(parsed),
        Err(refused) => {
            ::cypher_parser::parse(&format!("{ONE_ROW_CLAUSE} {query}")).map_err(|_| refused)
        }
    }
}

/// Why a query this data plane was handed could not be answered.
///
/// Each variant is a different fact about the REQUEST, which is why they are kept apart:
/// the operation reports each one through the modelled error that states it.
#[derive(Debug)]
pub enum QueryFailure {
    /// The text could not be READ as openCypher: the sentence stating what the parse
    /// stopped on, what the grammar could have continued the text with there, and where
    /// in the text that was ([`syntax_refusal`]).
    Malformed(String),
    /// The query was read and could not be EVALUATED — an unknown function, an argument
    /// of a type it cannot be applied to: the engine's own statement about the query it
    /// did read.
    Rejected(String),
    /// The query text is longer than this emulator reads
    /// ([`super::MAX_QUERY_STRING_CHARS`]).
    TooLong { characters: usize, ceiling: usize },
    /// The query text nests deeper than this emulator reads
    /// ([`MAX_QUERY_NESTING_DEPTH`]).
    TooDeep { depth: usize, ceiling: usize },
    /// The query text carries more tokens than one parse of this emulator's descends
    /// through ([`MAX_QUERY_TOKENS`]).
    TooManyTokens { tokens: usize, ceiling: usize },
    /// The result set is larger than this emulator answers with
    /// ([`MAX_QUERY_RESULT_ROWS`]).
    TooLarge { rows: usize, ceiling: usize },
    /// The query names a clause this data plane runs through neither of its engines
    /// ([`UNSUPPORTED_CLAUSES`]).
    Unsupported { clause: &'static str },
    /// The query asked for something the graph's own rules do not allow — a second
    /// element under one id, an element named longer than one is held.
    Constraint(String),
    /// The query references a `$name` parameter the request's `parameters` did not carry,
    /// so there is no data to bind it to.
    MissingParameter(String),
    /// The run, or the data it would have left behind, is past a ceiling this emulator
    /// holds: the sentence states which.
    OverLimit(String),
    /// The run could not be carried out at all — a failure of this service rather than
    /// anything the request said.
    Interrupted(String),
    /// The evaluation was stopped before it had answered: it asked for more of the graph
    /// than one query may read, the span it was given ran out, or the query was
    /// cancelled.
    Stopped(Stopped),
}

impl QueryFailure {
    /// How the engine's own refusal of `query` is reported.
    ///
    /// A text the engine could not READ is worded HERE rather than by the operation:
    /// this is where the query text is, and it runs off the request path, so composing
    /// the sentence — which asks the parser what its grammar admits at the position the
    /// parse stopped at, a fixed number of parses of a text already bounded by
    /// [`super::MAX_QUERY_STRING_CHARS`] and [`MAX_QUERY_NESTING_DEPTH`] — happens on the
    /// same thread, under the same permit, as the evaluation it stands in for.
    fn refused(query: &str, refused: ::cypher_parser::CypherError) -> Self {
        match refused {
            ::cypher_parser::CypherError::Syntax { message, position } => {
                Self::Malformed(syntax_refusal(query, &message, position))
            }
            ::cypher_parser::CypherError::Execution { message } => Self::Rejected(message),
        }
    }
}

/// The most rows one query's result may carry.
///
/// Emulator resource guard, NOT a quota AWS publishes: the data a graph holds is bounded
/// ([`super::MAX_NODES_PER_GRAPH`], [`super::MAX_EDGES_PER_GRAPH`]) but a pattern naming
/// several elements answers a row per COMBINATION of them. What keeps that amplification
/// from being allocated at all is [`MAX_QUERY_WORK_UNITS`], which the engine spends as it
/// builds those rows; this ceiling is the last check on what is rendered, and a result
/// past it is reported as the limit it met rather than truncated to look complete.
pub const MAX_QUERY_RESULT_ROWS: usize = 50_000;

/// The most candidate elements one evaluation may be handed.
///
/// Emulator resource guard, and the bound that DOMINATES the rows an evaluation can
/// build: the executor produces a row per candidate the provider hands it and asks again
/// for every row it carries into the next step, so an evaluation that would answer a
/// combinatorial number of rows is stopped as it is building them rather than after.
/// Set above the cost of reading a full graph end to end — one pass over
/// [`super::MAX_NODES_PER_GRAPH`] nodes plus one over [`super::MAX_EDGES_PER_GRAPH`]
/// edges, several times over — and far below the memory a combinatorial expansion would
/// take.
pub const MAX_QUERY_WORK_UNITS: usize = 200_000;

/// The deepest a query text may nest before it is read.
///
/// Emulator resource guard: the engine's parser descends through a nested expression, so
/// a text nesting arbitrarily deep would recurse arbitrarily deep on a caller's input.
/// The depth is counted off the text's own brackets ([`nesting_depth`]) BEFORE the parser
/// sees it, and set well above any query a local emulator is asked to run. Brackets are
/// not the only construct a recursive-descent parser descends through, so this bound is
/// one half of the pair that keeps the parse shallow: [`MAX_QUERY_TOKENS`] is the other.
pub const MAX_QUERY_NESTING_DEPTH: usize = 64;

/// The most tokens one query text may carry.
///
/// Emulator resource guard, and the other half of [`MAX_QUERY_NESTING_DEPTH`]: the
/// engine's parser is recursive-descent, so it descends not only through a text's
/// BRACKETS but through the prefix and infix constructs of its grammar — a chain of
/// negations, of conjunctions, of property accesses or of list items recurses with no
/// bracket anywhere in it, which a bracket depth cannot see. What bounds that descent
/// whatever the construct is simply how many tokens the text has at all: a parse descends
/// through a token at most once, so capping the count caps the recursion. The stack one
/// evaluation is given ([`QUERY_STACK_BYTES`]) leaves several kilobytes per token of this
/// count — room for the handful of frames a grammar's precedence levels take per token —
/// where the text length alone would allow tens of thousands of them. An overflowing
/// stack aborts the process rather than failing one request, which is why the count is
/// measured BEFORE the parser sees the text ([`token_count`]) — and every PREFIX of a
/// text that passed the bound is within it too, so the re-parses that word a refusal
/// ([`expected_alternatives`]) are bounded by the same count. Set well above the tokens
/// any query a local emulator is asked to run carries.
pub const MAX_QUERY_TOKENS: usize = 2_048;

/// How many queries this emulator evaluates AT ONCE.
///
/// Emulator resource guard: each evaluation holds a thread and a copy of the graph it
/// runs over, so without this bound a caller could start as many as it can open
/// connections. A request that finds every slot taken is shed as the throttle it is,
/// which is what keeps the host's threads and memory bounded by this number rather than
/// by the request rate.
pub const MAX_CONCURRENT_QUERIES: usize = 16;

/// The permits [`MAX_CONCURRENT_QUERIES`] is enforced with, held for the whole run of one
/// evaluation.
pub static QUERY_SLOTS: ::tokio::sync::Semaphore =
    ::tokio::sync::Semaphore::const_new(MAX_CONCURRENT_QUERIES);

/// The stack one evaluation is given.
///
/// The engine's parser and executor descend through a nested query, so the thread that
/// runs them is given a stack sized for the depth this service admits
/// ([`MAX_QUERY_NESTING_DEPTH`]) with room to spare, rather than left on whatever the
/// platform's default happens to be: an overflowing stack aborts the process, which is
/// not a failure one caller's query may cause.
pub const QUERY_STACK_BYTES: usize = 16 * 1024 * 1024;

/// How deep `query`'s brackets nest.
///
/// Not a parse — the engine owns that — but the one measurement that has to be taken
/// BEFORE one: how far a parser reading this text would descend. It is counted over the
/// ENGINE'S tokens where the text tokenises, so what this measures and what the parser
/// then recurses through cannot disagree: the lexer is what decides where a string
/// literal and a comment start and end, and a bracket inside either of them is not a
/// token at all.
///
/// A text the lexer CANNOT read tokenises to nothing, and the bound still has to hold
/// for it: a prefix of such a text is handed back to the parser when a refusal states
/// what could have continued it ([`expected_alternatives`]), and that prefix can lex
/// cleanly. So the brackets are then counted off the characters instead
/// ([`character_nesting`]), which can only OVER-state the depth of a text that is
/// already not a query — the safe direction for a bound whose job is to keep a parser
/// from descending through a caller's text.
#[must_use]
pub fn nesting_depth(query: &str) -> usize {
    let Ok(tokens) = ::cypher_parser::lexer::tokenize(query) else {
        return character_nesting(query);
    };
    let mut depth: usize = 0;
    let mut deepest: usize = 0;
    for token in &tokens {
        match token.kind {
            ::cypher_parser::lexer::TokenKind::LParen
            | ::cypher_parser::lexer::TokenKind::LBracket
            | ::cypher_parser::lexer::TokenKind::LBrace => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            ::cypher_parser::lexer::TokenKind::RParen
            | ::cypher_parser::lexer::TokenKind::RBracket
            | ::cypher_parser::lexer::TokenKind::RBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// How many tokens `query` carries: the count one parse of it can descend through.
///
/// Counted over the ENGINE'S own tokens where the text lexes, so what is measured and
/// what the parser then reads cannot disagree. A text the lexer CANNOT read is counted by
/// its CHARACTERS instead, which can only OVER-state the count — every token is at least
/// one character — the safe direction for a bound whose job is to keep a parser from
/// descending through a caller's text, and the same reading [`nesting_depth`] takes for
/// such a text.
#[must_use]
pub fn token_count(query: &str) -> usize {
    match ::cypher_parser::lexer::tokenize(query) {
        Ok(tokens) => tokens.len(),
        Err(_) => query.chars().count(),
    }
}

/// How deep the brackets of a text the lexer could not read nest, counted off its
/// CHARACTERS: every opening bracket of the language descends and every closing one
/// returns, wherever it stands.
fn character_nesting(query: &str) -> usize {
    let mut depth: usize = 0;
    let mut deepest: usize = 0;
    for character in query.chars() {
        match character {
            '(' | '[' | '{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// The clause keywords this data plane runs through NEITHER of its engines: the clauses
/// that would reach outside the graph — a procedure call, a bulk load from a file — and
/// the one that runs a clause per element of a list.
///
/// openCypher's updating clauses are NOT here: `ExecuteQuery` runs read and write
/// openCypher (`smithy_query(query="doc",shape="ExecuteQuery")`), and a query carrying
/// one is read and evaluated by the engine that has a production for it
/// ([`super::beyond_the_reading_engine`]).
///
/// Source: the clauses either engine evaluates are `MATCH`, `OPTIONAL MATCH`, `UNWIND`,
/// `WITH`, `CREATE`, `MERGE`, `SET`, `REMOVE` and `[DETACH] DELETE` before a `RETURN`, so
/// every other clause keyword is outside both. They are listed here to say WHICH one a
/// refused query named, because "this endpoint does not run that clause" and "this text
/// is not a query" are different answers to the caller.
const UNSUPPORTED_CLAUSES: &[&str] = &["CALL", "FOREACH", "LOAD"];

/// The clause of [`UNSUPPORTED_CLAUSES`] `query` names, if it names one.
///
/// Read off the engine's own token stream, so a keyword inside a string literal or a
/// comment is not one: the lexer has already decided what is a word of the query. A word
/// that follows a `.` is a property name, a word that follows `AS` is an alias the query
/// chose, and a word a `:` follows is a map key — none of which is a clause, so none of
/// them counts.
#[must_use]
pub fn unsupported_clause(query: &str) -> Option<&'static str> {
    use ::cypher_parser::lexer::TokenKind;
    let tokens = ::cypher_parser::lexer::tokenize(query).ok()?;
    for (at, token) in tokens.iter().enumerate() {
        let TokenKind::Ident(word) = &token.kind else {
            continue;
        };
        let names_something_else = match at.checked_sub(1).and_then(|before| tokens.get(before)) {
            Some(before) => match &before.kind {
                TokenKind::Dot => true,
                TokenKind::Ident(keyword) => keyword.eq_ignore_ascii_case(ALIAS_KEYWORD),
                _ => false,
            },
            None => false,
        };
        if names_something_else {
            continue;
        }
        if matches!(tokens.get(at + 1).map(|after| &after.kind), Some(TokenKind::Colon)) {
            continue;
        }
        if let Some(clause) = UNSUPPORTED_CLAUSES
            .iter()
            .find(|clause| word.eq_ignore_ascii_case(clause))
        {
            return Some(clause);
        }
    }
    None
}

/// The keyword a projection names a column by, after which a word is the caller's own
/// alias rather than a clause.
const ALIAS_KEYWORD: &str = "AS";

/// The rows a query could make from its own LITERALS, before any of the graph is read —
/// a WORST CASE, read off the token stream the engine's own lexer produces.
///
/// `UNWIND <list> AS x` makes one row per item of the list, so a query chaining several
/// multiplies them. Those rows are the ones no reading of the graph is charged for, so
/// the work budget the provider spends ([`MAX_QUERY_WORK_UNITS`]) never sees them, and
/// they have to be bounded from the text instead. What is measured is deliberately
/// INDEPENDENT of where each `UNWIND` reads from — a list written in place, a variable a
/// `WITH` bound to one, an alias of an alias — because the question is only how many rows
/// the text's OWN literals could expand to: the longest list literal the text contains,
/// raised to the number of `UNWIND` clauses that could each expand one. It saturates
/// rather than wrapping, so a text that would answer more rows than a `usize` counts
/// reports the largest count.
///
/// A list the query reads out of the GRAPH is not measurable from the text at all, and
/// is not measured here: those are charged to the work budget by the element, as the
/// provider hands them over ([`StoredGraph::property`]), which is what bounds the rows
/// an expansion of a stored list can build.
#[must_use]
pub fn literal_rows(query: &str) -> usize {
    use ::cypher_parser::lexer::TokenKind;
    let Ok(tokens) = ::cypher_parser::lexer::tokenize(query) else {
        return 1;
    };
    // The items of each list the text is inside of, innermost last: a `,` counts for the
    // list it sits directly in, so a list of lists counts its own items and not theirs.
    let mut open_lists: Vec<usize> = Vec::new();
    let mut longest_list: usize = 0;
    let mut expansions: u32 = 0;
    for (at, token) in tokens.iter().enumerate() {
        match &token.kind {
            TokenKind::LBracket => open_lists.push(1),
            TokenKind::RBracket => {
                if let Some(items) = open_lists.pop() {
                    longest_list = longest_list.max(items);
                }
            }
            TokenKind::Comma => {
                if let Some(items) = open_lists.last_mut() {
                    *items += 1;
                }
            }
            TokenKind::Ident(word) if word.eq_ignore_ascii_case(UNWIND_KEYWORD) => {
                // A word that follows `.` or `AS` names a property or an alias rather
                // than the clause, the same reading [`unsupported_clause`] applies.
                let names_the_clause = !matches!(
                    at.checked_sub(1).and_then(|before| tokens.get(before)).map(|before| &before.kind),
                    Some(TokenKind::Dot)
                ) && !matches!(
                    at.checked_sub(1).and_then(|before| tokens.get(before)).map(|before| &before.kind),
                    Some(TokenKind::Ident(keyword)) if keyword.eq_ignore_ascii_case(ALIAS_KEYWORD)
                );
                if names_the_clause {
                    expansions = expansions.saturating_add(1);
                }
            }
            _ => {}
        }
    }
    if expansions == 0 {
        // A query that expands no list makes one row from its literals: the projection.
        return 1;
    }
    longest_list.max(1).saturating_pow(expansions)
}

/// The clause that expands a list into rows, and the one source of rows the graph is
/// never asked about.
const UNWIND_KEYWORD: &str = "UNWIND";

/// Run `query` over `data` and answer the payload the operation reports, or why it could
/// not be answered.
///
/// Every step of reading and evaluating the query is the engine's: the text is lexed and
/// parsed by its parser ([`parse_query`]) and evaluated by its executor over the graph
/// [`StoredGraph`] exposes. What this adds is the bounds a caller-chosen input needs,
/// each taken BEFORE the work it caps — the text this emulator reads at most, how deep
/// that text may nest, the rows a query's own literals may make, the candidate elements
/// one evaluation may be handed and the span it may spend being handed them — and the
/// rendering of the result in this service's own wire form.
///
/// `deadline` is the instant the evaluation must stop ASKING at, so a query whose cost
/// diverges stops doing work rather than being left running after its caller has been
/// answered. It touches no store and takes no lock, so the caller runs it off the request
/// path.
pub fn run_query(
    data: &GraphData,
    query: &str,
    deadline: Instant,
) -> Result<Vec<u8>, QueryFailure> {
    run_query_until(data, query, deadline, &QueryCancellation::new())
}

/// [`run_query`], stopped as soon as `cancellation` is signalled: the run of a query the
/// store holds a record for, which `CancelQuery` can end while it is still asking.
///
/// The cancel is read at every question the evaluation asks the graph, so a cancelled
/// query stops doing work and is reported as the cancelled query it is rather than
/// answering results nobody is waiting for.
pub fn run_query_until(
    data: &GraphData,
    query: &str,
    deadline: Instant,
    cancellation: &QueryCancellation,
) -> Result<Vec<u8>, QueryFailure> {
    // Both text bounds are checked BEFORE the parser sees the text: `queryString` is
    // modelled as an unconstrained `String`, so these are what stop an arbitrarily large
    // or arbitrarily nested text from being parsed at all.
    let characters = query.chars().count();
    if characters > super::MAX_QUERY_STRING_CHARS {
        return Err(QueryFailure::TooLong {
            characters,
            ceiling: super::MAX_QUERY_STRING_CHARS,
        });
    }
    let depth = nesting_depth(query);
    if depth > MAX_QUERY_NESTING_DEPTH {
        return Err(QueryFailure::TooDeep {
            depth,
            ceiling: MAX_QUERY_NESTING_DEPTH,
        });
    }
    // A text's BRACKETS are not the only thing a recursive-descent parser descends
    // through, so the depth above is paired with the count of tokens the text carries at
    // all ([`MAX_QUERY_TOKENS`]): that is what bounds the descent of a chain of prefix or
    // infix constructs, which nests without a bracket in it.
    let tokens = token_count(query);
    if tokens > MAX_QUERY_TOKENS {
        return Err(QueryFailure::TooManyTokens {
            tokens,
            ceiling: MAX_QUERY_TOKENS,
        });
    }
    // The rows the query's own literals could expand to are counted from the TEXT and
    // refused before the engine builds any of them: they are rows the provider's budget
    // never sees, because the engine makes them without asking the graph anything.
    let literal_rows = literal_rows(query);
    if literal_rows > MAX_QUERY_RESULT_ROWS {
        return Err(QueryFailure::TooLarge {
            rows: literal_rows,
            ceiling: MAX_QUERY_RESULT_ROWS,
        });
    }
    let parsed = match parse_query(query) {
        Ok(parsed) => parsed,
        // A text the grammar could not read may be a well-formed query naming a clause
        // this engine has no grammar for, which is a different answer to the caller than
        // "this text is not a query": it is reported as the clause it named.
        Err(refused) => {
            return Err(match unsupported_clause(query) {
                Some(clause) => QueryFailure::Unsupported { clause },
                None => QueryFailure::refused(query, refused),
            });
        }
    };
    let graph = StoredGraph::over_within(data, MAX_QUERY_WORK_UNITS, deadline)
        .cancelled_by(cancellation.clone());
    let executed = ::cypher_parser::execute(&graph, &parsed);
    // The evaluation may have been stopped while it was building: what the engine was
    // left holding is a SHORT answer, so the limit that stopped it is reported and the
    // rows are dropped. Read BEFORE the engine's own answer, because an evaluation the
    // provider stopped answering may also leave the engine with an error about the short
    // view it was handed, and why the run did not finish is what the caller needs back.
    if let Some(stopped) = graph.stopped() {
        return Err(QueryFailure::Stopped(stopped));
    }
    let results = executed.map_err(|refused| QueryFailure::refused(query, refused))?;
    if results.rows.len() > MAX_QUERY_RESULT_ROWS {
        return Err(QueryFailure::TooLarge {
            rows: results.rows.len(),
            ceiling: MAX_QUERY_RESULT_ROWS,
        });
    }
    Ok(results_payload(&results, data))
}

/// How long one query may run for: what the request asked for, clamped to the span a
/// graph's queries run under ([`super::DEFAULT_QUERY_TIMEOUT`]).
///
/// "Specifies the query timeout duration, in milliseconds"
/// (`smithy_query(query="operation",operation="ExecuteQuery")` ->
/// input.queryTimeoutMilliseconds) is the caller's choice, so it is honoured — but only
/// DOWNWARD: the documented timeout of a graph's queries is the maximum the service runs
/// one for, and a caller cannot ask for longer. A value at or below zero names no span to
/// run in and is read as the request having asked for nothing.
#[must_use]
pub fn query_run_timeout(requested_milliseconds: Option<i32>) -> ::std::time::Duration {
    let requested = requested_milliseconds
        .filter(|asked| *asked > 0)
        .map(|asked| ::std::time::Duration::from_millis(u64::from(asked.unsigned_abs())));
    requested.map_or(super::DEFAULT_QUERY_TIMEOUT, |asked| {
        asked.min(super::DEFAULT_QUERY_TIMEOUT)
    })
}

/// How many ELEMENTS a stored value carries, through the lists and maps it nests: what
/// an expansion of that value could make rows over.
///
/// A scalar carries none — it makes one row wherever it stands, which the question that
/// returned it is already charged for. A list or a map carries its own entries plus
/// whatever those nest, counted saturating so no stored value can make this wrap.
fn value_elements(stored: &::serde_json::Value) -> usize {
    match stored {
        ::serde_json::Value::Array(items) => items.iter().fold(items.len(), |counted, item| {
            counted.saturating_add(value_elements(item))
        }),
        ::serde_json::Value::Object(entries) => {
            entries.values().fold(entries.len(), |counted, entry| {
                counted.saturating_add(value_elements(entry))
            })
        }
        _ => 0,
    }
}

/// One stored property as the engine's value domain holds it.
///
/// Every JSON form a property can be written with has a counterpart except a FRACTIONAL
/// number, which openCypher's integer has no form for: it is read as the text of its own
/// JSON number, so the value is reported as written rather than truncated to a different
/// number (see the module docs).
fn engine_value(stored: &::serde_json::Value) -> CypherValue {
    match stored {
        ::serde_json::Value::Null => CypherValue::Null,
        ::serde_json::Value::Bool(flag) => CypherValue::Bool(*flag),
        ::serde_json::Value::Number(number) => number
            .as_i64()
            .map_or_else(|| CypherValue::Str(number.to_string()), CypherValue::Int),
        ::serde_json::Value::String(text) => CypherValue::Str(text.clone()),
        ::serde_json::Value::Array(items) => {
            CypherValue::List(items.iter().map(engine_value).collect())
        }
        ::serde_json::Value::Object(entries) => CypherValue::Map(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), engine_value(value)))
                .collect(),
        ),
    }
}

/// The member a result row names a column by: an openCypher result is a list of rows,
/// each an object keyed by the query's own return items.
const RESULTS_MEMBER: &str = "results";

/// The members a node is spelled with on this service's wire: its id, the kind of
/// element it is, its labels and its properties.
const NODE_ID_MEMBER: &str = "~id";
const NODE_ENTITY_TYPE_MEMBER: &str = "~entityType";
const NODE_LABELS_MEMBER: &str = "~labels";
const NODE_PROPERTIES_MEMBER: &str = "~properties";

/// The `~entityType` of a node.
const NODE_ENTITY_TYPE: &str = "node";

/// One result set as the operation's `payload` carries it: `{"results": [ … ]}`, one
/// object per row keyed by the query's return items, in the order the engine produced
/// them.
///
/// `data` is the graph the query ran over, so a row carrying a NODE reports that node as
/// this service reports one — its id, its labels and its properties, read from the store
/// rather than from the engine's display projection of it.
#[must_use]
fn results_payload(result: &ResultSet, data: &GraphData) -> Vec<u8> {
    let rows: Vec<::serde_json::Value> = result
        .rows
        .iter()
        .map(|row| {
            let mut object = ::serde_json::Map::new();
            for (column, value) in result.columns.iter().zip(row) {
                object.insert(column.clone(), wire_value(value, data));
            }
            ::serde_json::Value::Object(object)
        })
        .collect();
    rows_payload(&rows)
}

/// The `payload` carrying `rows`: `{"results": [ … ]}`, one object per row keyed by the
/// query's own return items, in the order the evaluation produced them.
///
/// The one place the member a result set is carried in is spelled, so a query answered by
/// either of this data plane's engines is answered in the same envelope.
#[must_use]
pub fn rows_payload(rows: &[::serde_json::Value]) -> Vec<u8> {
    ::serde_json::json!({ RESULTS_MEMBER: rows }).to_string().into_bytes()
}

/// One value the engine produced, as this service spells it on the wire.
fn wire_value(value: &CypherValue, data: &GraphData) -> ::serde_json::Value {
    match value {
        CypherValue::Null => ::serde_json::Value::Null,
        CypherValue::Bool(flag) => ::serde_json::Value::Bool(*flag),
        CypherValue::Int(number) => ::serde_json::Value::from(*number),
        CypherValue::Str(text) => ::serde_json::Value::String(text.clone()),
        CypherValue::Node { id, label, .. } => node_value(id, label, data),
        CypherValue::List(items) => ::serde_json::Value::Array(
            items.iter().map(|item| wire_value(item, data)).collect(),
        ),
        CypherValue::Map(entries) => ::serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, entry)| (key.clone(), wire_value(entry, data)))
                .collect(),
        ),
    }
}

/// One bound node, spelled from the STORE: the id it is addressed by, every label it
/// carries and every property set on it.
///
/// A node the store no longer holds — a graph written between the query and this
/// rendering could not reach here, since both read one locked store — is reported with
/// the engine's own projection of it, so a row is never dropped from a result set.
fn node_value(id: &str, label: &str, data: &GraphData) -> ::serde_json::Value {
    let (labels, properties) = match data.nodes.get(id) {
        Some(stored) => (
            stored.labels.iter().cloned().collect::<Vec<String>>(),
            stored
                .properties
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<::serde_json::Map<String, ::serde_json::Value>>(),
        ),
        None => (
            vec![label.to_string()],
            ::serde_json::Map::new(),
        ),
    };
    ::serde_json::json!({
        NODE_ID_MEMBER: id,
        NODE_ENTITY_TYPE_MEMBER: NODE_ENTITY_TYPE,
        NODE_LABELS_MEMBER: labels,
        NODE_PROPERTIES_MEMBER: properties,
    })
}

/// The sentence a query the parser could not read is refused with: what was found where
/// the parser could not continue, the ALTERNATIVES that could have stood there instead,
/// and WHERE in the query text that was — as a line, a column and an offset, all three
/// counted in characters of the submitted text.
///
/// The position is reported in all three forms because a client fixing a query needs the
/// line and column, while the offset is what the parser itself decided at. What could
/// have stood there is the GRAMMAR's statement and not this service's, so it is not
/// worded here either: it is asked of the engine, by reading the caller's own text up to
/// the position and offering the parser each construct the language is written with
/// ([`expected_alternatives`]). Only when the grammar admits nothing at all at that
/// position does the engine's own report of what it was looking for stand in its place.
#[must_use]
pub fn syntax_refusal(query: &str, message: &str, position: usize) -> String {
    // A position past the end of the text, or inside a multi-byte character, is a
    // position this wording cannot split the text at: it is taken back to the nearest
    // character boundary at or before it, so no caller's text can make this panic.
    let boundary = (0..=position.min(query.len()))
        .rev()
        .find(|at| query.is_char_boundary(*at))
        .unwrap_or_default();
    let before = query.get(..boundary).unwrap_or_default();
    // The position is counted in CHARACTERS of the submitted text, so what a refusal
    // states is what a reader of that text counts to rather than how many bytes it
    // happens to be written in.
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map_or(before, |start| &before[start + 1..])
        .chars()
        .count()
        + 1;
    let offset = before.chars().count();
    let found = found_input(query, boundary);
    let expected = expected_alternatives(query, boundary)
        .unwrap_or_else(|| engine_expectation(message).to_string());
    format!(
        "Invalid input '{found}': expected {expected} (line {line}, column {column} \
         (offset: {offset}))"
    )
}

/// What a parser names the end of the text it was reading, which is what a query stopped
/// at its own end is reported with instead of a character.
const END_OF_INPUT: &str = "EOI";

/// The input a refusal reports it could not use: the CHARACTER the parse stopped on, or
/// [`END_OF_INPUT`] when it stopped at the end of the text.
///
/// A control character is reported escaped, so a refusal cannot carry a line break of
/// the caller's own choosing into whatever renders it.
fn found_input(query: &str, offset: usize) -> String {
    let Some(found) = query.get(offset..).and_then(|rest| rest.chars().next()) else {
        return END_OF_INPUT.to_string();
    };
    if found.is_control() {
        return found.escape_debug().to_string();
    }
    found.to_string()
}

/// What the engine said it was looking for, with the `expected ` the sentence already
/// carries taken off its front, so the refusal states it once.
fn engine_expectation(message: &str) -> &str {
    message.strip_prefix(EXPECTED_PREFIX).unwrap_or(message)
}

/// How a parser's report, and this sentence, introduce what could have stood at the
/// position a parse stopped.
const EXPECTED_PREFIX: &str = "expected ";

/// One construct a query may continue with, as a refusal NAMES it and as the grammar is
/// ASKED about it.
#[derive(Debug)]
struct Continuation {
    /// How a refusal names this alternative: a literal token in quotes, as the grammar
    /// spells it; a production of the grammar by the production's own name; and anything
    /// else by the construct it is.
    name: &'static str,
    /// The shortest text that starts this construct, or EMPTY for a production decided by
    /// the position alone (see [`Continuation::beside`]). It is appended to the text the
    /// parser had read and the whole is parsed again: the construct may stand at that
    /// position exactly when the parser reads past it rather than stopping where it
    /// stopped before.
    ///
    /// One construct may be asked about with SEVERAL probes — one entry each, under the
    /// same name — because a construct can be reached from a position in more than one
    /// way: a relationship pattern continues a pattern whose node pattern is complete,
    /// and continues one whose node pattern is still open once that pattern is closed.
    /// The alternative is listed once, at the first probe that reaches it.
    probe: &'static str,
    /// The alternatives this one stands BESIDE: it is admitted exactly where every one of
    /// them is.
    ///
    /// What a production with no text of its own is decided by — the grammar's own name
    /// for a part of a construct that is admitted wherever the construct is, which a
    /// parse cannot be asked about separately because the two start the same way.
    beside: &'static [&'static str],
    /// Whether the probe is spelled as a WORD. A position that takes a variable takes
    /// every word, so naming each keyword there would say nothing about the grammar:
    /// these are asked about only where no variable may stand.
    a_word: bool,
}

impl Continuation {
    /// A construct named by what it is, or by the token it is, asked about everywhere.
    const fn construct(name: &'static str, probe: &'static str) -> Self {
        Self { name, probe, beside: &[], a_word: false }
    }

    /// A production of the grammar admitted wherever every alternative of `beside` is —
    /// no text of its own is parsed for it (see [`Continuation::beside`]).
    const fn production(name: &'static str, beside: &'static [&'static str]) -> Self {
        Self { name, probe: "", beside, a_word: false }
    }

    /// A construct spelled as a word of the language, asked about only where a variable
    /// cannot stand (see [`Continuation::a_word`]).
    const fn keyword(name: &'static str) -> Self {
        Self { name, probe: name, beside: &[], a_word: true }
    }
}

/// The text that stands for a VARIABLE, whose acceptance decides whether the keywords
/// are asked about at all.
const VARIABLE_PROBE: &str = "variable";

/// How a refusal names the two alternatives that stand beside the parts of a NODE
/// PATTERN: the keyword a node pattern's own continuation is spelled with, and the map
/// form of the properties production.
const COPY_ALTERNATIVE: &str = "COPY";
const MAP_LITERAL_ALTERNATIVE: &str = "MapLiteral";

/// How a refusal names the parts of a node pattern the two above stand beside.
const NODE_LABELS_ALTERNATIVE: &str = "node labels";
const PROPERTIES_ALTERNATIVE: &str = "a property map";
const CLOSE_PAREN_ALTERNATIVE: &str = "')'";

/// Every construct one position of a query is asked about, in the order a refusal lists
/// them.
///
/// Source: the constructs of the openCypher grammar this service's parser reads — the two
/// forms of text a parse skips (whitespace and a comment), the parts a pattern is built
/// from (`Variable`, `NodeLabels`, `Properties`/`MapLiteral`, `NodePattern`,
/// `RelationshipPattern`), the keyword a node pattern's own continuation is spelled with,
/// the literals an expression is written with, the tokens that close or continue one, and
/// the clause and operator keywords of the subset the reading engine accepts (see the
/// module docs, and [`UNSUPPORTED_CLAUSES`] for the clauses outside both engines). Each is
/// named as the grammar spells it: a production by its own name, a token in quotes.
///
/// WHICH of them may stand at a given position is not stated here and could not be: it
/// is decided by parsing the caller's own text with each probe appended
/// ([`expected_alternatives`]), so the list a refusal carries is the grammar's answer for
/// that one position in that one text.
const CONTINUATIONS: &[Continuation] = &[
    Continuation::construct("whitespace", " "),
    Continuation::construct("comment", "/* comment */"),
    Continuation::production(
        COPY_ALTERNATIVE,
        &[NODE_LABELS_ALTERNATIVE, CLOSE_PAREN_ALTERNATIVE],
    ),
    Continuation::construct("a variable", VARIABLE_PROBE),
    Continuation::construct(NODE_LABELS_ALTERNATIVE, ":Label"),
    Continuation::construct(PROPERTIES_ALTERNATIVE, "{ key: 0 }"),
    Continuation::construct(CLOSE_PAREN_ALTERNATIVE, ")"),
    Continuation::production(MAP_LITERAL_ALTERNATIVE, &[PROPERTIES_ALTERNATIVE]),
    Continuation::construct("a relationship pattern", "-[relationship]->(node)"),
    Continuation::construct("a node pattern", "(node)"),
    Continuation::construct("a number", "0"),
    Continuation::construct("a string", "'text'"),
    Continuation::construct("a list", "[0]"),
    Continuation::construct("']'", "]"),
    Continuation::construct("'}'", "}"),
    Continuation::construct("','", ","),
    Continuation::construct("'.'", "."),
    Continuation::keyword("MATCH"),
    Continuation::keyword("OPTIONAL MATCH"),
    Continuation::keyword("UNWIND"),
    Continuation::keyword("WITH"),
    Continuation::keyword("WHERE"),
    Continuation::keyword("RETURN"),
    Continuation::keyword("AS"),
    Continuation::keyword("DISTINCT"),
    Continuation::keyword("ORDER BY"),
    Continuation::keyword("SKIP"),
    Continuation::keyword("LIMIT"),
    Continuation::keyword("AND"),
    Continuation::keyword("OR"),
    Continuation::keyword("NOT"),
    // A relationship pattern also continues a pattern whose node pattern is still open,
    // once that node pattern is closed, so the position inside one admits it too — last,
    // because it is reached through the close of the pattern the position is inside.
    Continuation::construct("a relationship pattern", ")-[relationship]->(node)"),
];

/// What the grammar allows at `offset` of `query`, worded as a refusal lists
/// alternatives: `a, b or c`.
///
/// The question is asked of the PARSER, never answered from a table: the text the parser
/// had already read is `query` up to `offset`, so each probe of [`CONTINUATIONS`] is
/// appended to exactly that text and the whole is parsed again ([`continues`]). What
/// comes back is the set of constructs the grammar can continue the caller's own text
/// with at that position — the ones the parse that failed was looking for — and then the
/// productions that stand beside them, which are admitted exactly where they are.
///
/// `None` when the grammar admits nothing there, which is what a text the lexer itself
/// could not read looks like: there is then no alternative to list, and the engine's own
/// report of what it wanted stands instead.
#[must_use]
pub fn expected_alternatives(query: &str, offset: usize) -> Option<String> {
    let read = query.get(..offset)?;
    // The text read so far is handed BACK to the parser below, so the bound on how deep
    // a text this service parses holds here too, measured on that prefix: it is a text
    // of the caller's own choosing, and nothing may descend through it past the depth
    // one query may nest. A prefix past the bound is left to the engine's own report of
    // what it was looking for.
    if nesting_depth(read) > MAX_QUERY_NESTING_DEPTH {
        return None;
    }
    let a_variable_fits = continues(read, VARIABLE_PROBE);
    // Which PROBE reached its construct, asked one parse at a time; a construct asked
    // about with several probes is reached by the first of them that gets past.
    let reached: Vec<bool> = CONTINUATIONS
        .iter()
        .map(|continuation| {
            !continuation.probe.is_empty()
                && !(continuation.a_word && a_variable_fits)
                && continues(read, continuation.probe)
        })
        .collect();
    let admitted: Vec<&'static str> = CONTINUATIONS
        .iter()
        .zip(&reached)
        .filter(|(_, reached)| **reached)
        .map(|(continuation, _)| continuation.name)
        .collect();
    let mut named: Vec<&'static str> = Vec::new();
    for (at, continuation) in CONTINUATIONS.iter().enumerate() {
        let listed = if continuation.probe.is_empty() {
            !continuation.beside.is_empty()
                && continuation
                    .beside
                    .iter()
                    .all(|beside| admitted.contains(beside))
        } else {
            reached.get(at).copied().unwrap_or_default()
        };
        if listed && !named.contains(&continuation.name) {
            named.push(continuation.name);
        }
    }
    match named.split_last() {
        None => None,
        Some((last, [])) => Some((*last).to_string()),
        Some((last, before)) => Some(format!("{} or {last}", before.join(", "))),
    }
}

/// Whether the grammar can continue the text `read` with `probe`.
///
/// It can exactly when a parse of the two together gets PAST the probe: either it reads
/// the whole text, or it stops at or after the probe's end, which is a position the probe
/// itself was read to. A parse that stops where `read` already ended was stopped by the
/// probe, so that construct cannot stand there.
///
/// A bare projection is read behind the clause that binds one row, exactly as
/// [`parse_query`] reads one, so the alternatives of such a query are the ones ITS
/// reading allows rather than the ones a text without a projection would allow.
fn continues(read: &str, probe: &str) -> bool {
    let offered = format!("{read}{probe}");
    let through = offered.len();
    if parses_through(&offered, through) {
        return true;
    }
    let behind = format!("{ONE_ROW_CLAUSE} {offered}");
    parses_through(&behind, through + ONE_ROW_CLAUSE.len() + 1)
}

/// Whether a parse of `text` read it to at least `through`.
fn parses_through(text: &str, through: usize) -> bool {
    match ::cypher_parser::parse(text) {
        Ok(_) => true,
        Err(::cypher_parser::CypherError::Syntax { position, .. }) => position >= through,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{GraphEdge, GraphNode};

    /// A graph holding two cities joined by one route, written through the store's own
    /// writers — which is where every write of a graph's data goes.
    fn two_cities() -> GraphData {
        let mut data = GraphData::empty(::std::time::SystemTime::UNIX_EPOCH);
        for (id, name) in [("c1", "Berlin"), ("c2", "Paris")] {
            data.insert_node(
                id,
                GraphNode {
                    labels: BTreeSet::from([String::from("city")]),
                    properties: BTreeMap::from([(
                        String::from("name"),
                        ::serde_json::Value::String(name.to_string()),
                    )]),
                },
                ::std::time::SystemTime::UNIX_EPOCH,
            )
            .expect("the node fits the store");
        }
        data.insert_edge(
            "r1",
            GraphEdge {
                label: String::from("route"),
                from_node: String::from("c1"),
                to_node: String::from("c2"),
                properties: BTreeMap::new(),
            },
            ::std::time::SystemTime::UNIX_EPOCH,
        )
        .expect("the edge fits the store");
        data
    }

    /// A graph read with the budget and the span one query is given, which is what a
    /// served request runs under.
    fn within_the_usual_bounds(data: &GraphData) -> StoredGraph<'_> {
        StoredGraph::over_within(data, MAX_QUERY_WORK_UNITS, soon())
    }

    /// A deadline no test's evaluation can reach, so a row measures what it is about
    /// rather than how fast the host runs.
    fn soon() -> Instant {
        Instant::now() + ::std::time::Duration::from_secs(600)
    }

    /// RULE: the engine reads the graph through the trait, so adjacency answers the
    /// edges the store holds — in both directions, and nothing for a type it has none of.
    #[test]
    fn adjacency_answers_the_stored_edges() {
        let data = two_cities();
        let graph = within_the_usual_bounds(&data);
        let berlin = graph.scan(&[String::from("city")]);
        assert_eq!(
            berlin.len(),
            2,
            "case \"both cities\": expected 2 nodes, got {berlin:?}"
        );
        let from = graph.expand(0, "route");
        assert_eq!(
            from,
            vec![1],
            "case \"the outgoing route\": expected node 1, got {from:?}"
        );
        let into = graph.expand_in(1, "route");
        assert_eq!(
            into,
            Some(vec![0]),
            "case \"the incoming route\": expected node 0, got {into:?}"
        );
        let absent = graph.expand(0, "flight");
        assert!(
            absent.is_empty(),
            "case \"a type the graph has no edge of\": expected none, got {absent:?}"
        );
    }

    /// RULE: an evaluation is handed at most the candidates one query may read, and it
    /// is STOPPED when it asks for more — the engine is answered nothing from then on,
    /// and the limit is reported rather than the short result it was left holding.
    #[test]
    fn an_evaluation_that_asks_for_too_much_is_stopped() {
        let data = two_cities();
        // One unit buys the first question and nothing it answers, so the scan the
        // simplest pattern starts from cannot be paid for.
        let pinched = StoredGraph::over_within(&data, 1, soon());
        let parsed = parse_query("MATCH (c:city) RETURN c.name AS name").expect("a parse");
        let answered = ::cypher_parser::execute(&pinched, &parsed).expect("an evaluation");
        assert_eq!(
            pinched.stopped(),
            Some(Stopped::Exhausted),
            "case \"a budget of one element\": expected the evaluation to be stopped"
        );
        assert!(
            answered.rows.is_empty(),
            "case \"a budget of one element\": the engine is answered nothing once the \
             budget is spent, got {:?}",
            answered.rows
        );
    }

    /// RULE: an evaluation stops ASKING at its deadline, so a query whose cost diverges
    /// stops doing work rather than running on; the span is reported as the timeout it is.
    #[test]
    fn an_evaluation_past_its_deadline_is_stopped() {
        let data = two_cities();
        // A deadline already behind us: the first question the engine asks is the one
        // that finds it.
        let expired = StoredGraph::over_within(&data, MAX_QUERY_WORK_UNITS, Instant::now());
        let parsed = parse_query("MATCH (c:city) RETURN c.name AS name").expect("a parse");
        let answered = ::cypher_parser::execute(&expired, &parsed).expect("an evaluation");
        assert_eq!(
            expired.stopped(),
            Some(Stopped::TimedOut),
            "case \"a deadline already passed\": expected the evaluation to be stopped"
        );
        assert!(
            answered.rows.is_empty(),
            "case \"a deadline already passed\": expected no rows, got {:?}",
            answered.rows
        );
        let reported = run_query(&data, "MATCH (c:city) RETURN c.name AS name", Instant::now());
        assert!(
            matches!(reported, Err(QueryFailure::Stopped(Stopped::TimedOut))),
            "case \"a query run past its deadline\": expected the timeout to be reported, \
             got {reported:?}"
        );
    }

    /// RULE: a query the service has accepted a cancel for stops ASKING the graph, so a
    /// cancelled run does no more work and is reported as the cancelled query it is
    /// rather than answering the short result the engine was left holding.
    ///
    /// The rows span what the rule generalises over: a query that reads the graph through
    /// a label scan and one that reads it through a traversal, each with the cancel
    /// signalled before the run and again on a handle nothing signalled.
    #[test]
    fn a_cancelled_run_stops_asking_and_is_reported_as_cancelled() {
        let data = two_cities();
        // (case, the query text) — one that reads the graph through a label scan and one
        // that reads it through a traversal: the cancel is read at every question either
        // asks.
        let rows = [
            ("a label scan", "MATCH (c:city) RETURN c.name AS name"),
            (
                "a traversal",
                "MATCH (a:city)-[:route]->(b:city) RETURN b.name AS name",
            ),
        ];
        for (case, query) in rows {
            for signalled in [true, false] {
                let cancellation = QueryCancellation::new();
                if signalled {
                    cancellation.signal();
                }
                let reported = run_query_until(&data, query, soon(), &cancellation);
                let cancelled =
                    matches!(reported, Err(QueryFailure::Stopped(Stopped::Cancelled)));
                assert_eq!(
                    cancelled, signalled,
                    "case {case:?} with the cancel signalled {signalled}: expected \
                     cancelled {signalled}, got {reported:?}"
                );
            }
        }
    }

    /// RULE: how far one parse of a text can descend is bounded before the parser sees
    /// it, by the text's bracket depth AND by the number of tokens it carries at all —
    /// because a chain of prefix or infix constructs nests with no bracket in it, and an
    /// overflowing stack would end the process rather than the request.
    ///
    /// The rows span the constructs that nest without a bracket: a negation chain, a
    /// conjunction chain, a property-access chain and a list of items, each past the
    /// token bound and each well inside the bracket bound; and the same shapes at a
    /// length an ordinary query reaches, which must NOT be refused for their count.
    #[test]
    fn a_text_is_bounded_by_its_token_count_as_well_as_by_its_brackets() {
        let data = two_cities();
        // (case, the text's unbracketed chain, how many of it the row repeats)
        let chains = [
            ("a negation chain", "NOT "),
            ("a conjunction chain", "true AND "),
            ("a property-access chain", "c.a"),
            ("a list continuation", "1, "),
        ];
        for (case, chain) in chains {
            // Past the bound: a text whose tokens outnumber what one parse descends
            // through is refused for the count, with no bracket in it to be refused for.
            let oversized = format!("RETURN {}true", chain.repeat(MAX_QUERY_TOKENS));
            assert_eq!(
                nesting_depth(&oversized),
                0,
                "case {case:?}: the row must carry no brackets, so only the token count \
                 can bound it"
            );
            let refused = run_query(&data, &oversized, soon());
            assert!(
                matches!(
                    refused,
                    Err(QueryFailure::TooManyTokens {
                        ceiling: MAX_QUERY_TOKENS,
                        ..
                    })
                ),
                "case {case:?} past the bound: expected the token count to be reported, \
                 got {refused:?}"
            );

            // At a length an ordinary query reaches, the count refuses nothing: whatever
            // the engine answers, it is not this bound.
            let ordinary = format!("RETURN {}true", chain.repeat(4));
            let answered = run_query(&data, &ordinary, soon());
            assert!(
                !matches!(answered, Err(QueryFailure::TooManyTokens { .. })),
                "case {case:?} at an ordinary length: the token bound must not refuse it, \
                 got {answered:?}"
            );
        }

        // A text the lexer cannot read is counted by its characters, which can only
        // over-state the tokens it carries — so an enormous unreadable text is refused
        // for its count rather than handed to the parser.
        let unreadable = "$".repeat(MAX_QUERY_TOKENS + 1);
        let refused = run_query(&data, &unreadable, soon());
        assert!(
            matches!(refused, Err(QueryFailure::TooManyTokens { .. })),
            "case \"an unreadable text past the bound\": expected the token count to be \
             reported, got {refused:?}"
        );
    }

    /// RULE: how deep a text nests is measured over the ENGINE'S OWN tokens before it
    /// reads them, so a bracket inside a string literal or a comment nests nothing and a
    /// bracket after one still counts; a text past the depth this service reads is
    /// refused before it is parsed.
    #[test]
    fn a_query_text_is_refused_past_the_depth_it_may_nest() {
        // (case, the text, the depth it nests)
        let rows: &[(&str, String, usize)] = &[
            ("no brackets at all", String::from("MATCH n RETURN n"), 0),
            (
                "one node pattern",
                String::from("MATCH (n) RETURN n"),
                1,
            ),
            (
                "a property map inside a pattern",
                String::from("MATCH (n {name: 'a'}) RETURN n"),
                2,
            ),
            (
                "brackets inside a string literal",
                String::from("MATCH (n) RETURN '((((((((((' AS text"),
                1,
            ),
            (
                "a list of lists",
                String::from("UNWIND [[1, 2], [3]] AS row RETURN row"),
                2,
            ),
            (
                "brackets after a quote inside a line comment",
                format!(
                    "MATCH (n) // '\n RETURN {}0{} AS deep",
                    "(".repeat(70),
                    ")".repeat(70)
                ),
                70,
            ),
            (
                "brackets inside a block comment",
                String::from("MATCH (n) /* ((((( */ RETURN n"),
                1,
            ),
            (
                "at the depth this service reads",
                format!(
                    "MATCH (n) RETURN {}0{} AS deep",
                    "(".repeat(MAX_QUERY_NESTING_DEPTH),
                    ")".repeat(MAX_QUERY_NESTING_DEPTH)
                ),
                MAX_QUERY_NESTING_DEPTH,
            ),
            (
                "one level past it",
                format!(
                    "MATCH (n) RETURN {}0{} AS deep",
                    "(".repeat(MAX_QUERY_NESTING_DEPTH + 1),
                    ")".repeat(MAX_QUERY_NESTING_DEPTH + 1)
                ),
                MAX_QUERY_NESTING_DEPTH + 1,
            ),
        ];
        let data = GraphData::empty(::std::time::SystemTime::UNIX_EPOCH);
        for (case, query, expected) in rows {
            let depth = nesting_depth(query);
            assert_eq!(
                depth, *expected,
                "case {case:?}: expected a depth of {expected}, got {depth}"
            );
            if depth <= MAX_QUERY_NESTING_DEPTH {
                continue;
            }
            let refused = run_query(&data, query, soon());
            assert!(
                matches!(refused, Err(QueryFailure::TooDeep { .. })),
                "case {case:?}: expected the depth refusal, got {refused:?}"
            );
        }
    }

    /// RULE: the rows a query's own LITERALS could expand to are counted from the text
    /// and refused before the engine builds any of them — whatever the `UNWIND` reads
    /// from, a list written in place or a variable a `WITH` bound to one — while a query
    /// whose literals stay inside the ceiling is answered.
    #[test]
    fn literal_rows_are_bounded_before_the_engine_builds_them() {
        let data = GraphData::empty(::std::time::SystemTime::UNIX_EPOCH);
        /// A list literal of `items` integers, as a query text carries one.
        fn list(items: usize) -> String {
            let elements: Vec<String> = (0..items).map(|item| item.to_string()).collect();
            format!("[{}]", elements.join(","))
        }
        // (case, the query, the rows its literals could make, whether it is answered)
        let rows: Vec<(&str, String, usize, bool)> = vec![
            (
                "one small list",
                format!("UNWIND {} AS a RETURN a", list(3)),
                3,
                true,
            ),
            (
                "two small lists",
                format!("UNWIND {} AS a UNWIND {} AS b RETURN a, b", list(3), list(4)),
                16,
                true,
            ),
            (
                "a projection that reads nothing",
                String::from("RETURN 1 AS one"),
                1,
                true,
            ),
            (
                "a list of lists",
                String::from("UNWIND [[1, 2], [3, 4]] AS row RETURN row"),
                2,
                true,
            ),
            (
                "a pattern carrying no list at all",
                String::from("MATCH (c:city) RETURN c"),
                1,
                true,
            ),
            (
                "two lists whose product is past the ceiling",
                format!(
                    "UNWIND {} AS a UNWIND {} AS b RETURN a, b",
                    list(300),
                    list(300)
                ),
                90_000,
                false,
            ),
            (
                "one list expanded three times through a binding",
                format!(
                    "WITH {} AS big UNWIND big AS a UNWIND big AS b UNWIND big AS c \
                     RETURN a, b, c",
                    list(100)
                ),
                1_000_000,
                false,
            ),
        ];
        for (case, query, expected, answered) in &rows {
            let counted = literal_rows(query);
            assert_eq!(
                counted, *expected,
                "case {case:?}: expected {expected} literal row(s) at worst, got {counted}"
            );
            let run = run_query(&data, query, soon());
            if *answered {
                assert!(
                    run.is_ok(),
                    "case {case:?}: expected the query to be answered, got {run:?}"
                );
                continue;
            }
            assert!(
                matches!(run, Err(QueryFailure::TooLarge { .. })),
                "case {case:?}: expected the row ceiling to refuse it, got {run:?}"
            );
        }
    }

    /// RULE: a well-formed query naming a clause NEITHER of this data plane's engines
    /// runs is reported as THAT clause, not as a text that does not parse — and a word
    /// that is not a clause (a property name, a map key, a word inside a string or a
    /// comment) is not one. An UPDATING clause is not one of them: it is a clause this
    /// endpoint runs (`super::beyond_the_reading_engine`).
    #[test]
    fn a_clause_the_engine_does_not_run_is_named() {
        // (case, the query, the clause it names)
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("a create", "CREATE (n:Person) RETURN n", None),
            ("a merge", "MERGE (n:Person) RETURN n", None),
            (
                "a write to a property",
                "MATCH (n) SET n.name = 'a' RETURN n",
                None,
            ),
            ("a delete", "MATCH (n) DELETE n RETURN n", None),
            (
                "a detach delete",
                "MATCH (n) DETACH DELETE n RETURN n",
                None,
            ),
            (
                "a clause per element of a list",
                "MATCH (n) FOREACH (x IN [1] | SET n.seen = x) RETURN n",
                Some("FOREACH"),
            ),
            (
                "a procedure call",
                "CALL dbms.listConfig() YIELD name RETURN name",
                Some("CALL"),
            ),
            (
                "a bulk load",
                "LOAD CSV FROM 'file:///etc/passwd' AS line RETURN line",
                Some("LOAD"),
            ),
            (
                "a property whose name reads like a clause",
                "MATCH (n) RETURN n.set AS set",
                None,
            ),
            (
                "a map key that reads like a clause",
                "MATCH (n {create: 'a'}) RETURN n",
                None,
            ),
            (
                "a string that reads like a clause",
                "MATCH (n) RETURN 'DELETE' AS text",
                None,
            ),
            (
                "a comment that reads like a clause",
                "MATCH (n) // CREATE\n RETURN n",
                None,
            ),
            ("a query naming no clause at all", "MATCH (n) RETURN n", None),
        ];
        for (case, query, expected) in rows {
            let named = unsupported_clause(query);
            assert_eq!(
                named, *expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
    }

    /// RULE: a query runs for the span the request asked for, clamped DOWNWARD to the
    /// span a graph's queries run under, and a request that asks for no usable span runs
    /// under that documented one.
    #[test]
    fn a_requested_timeout_is_honoured_only_downward() {
        // (case, the `queryTimeoutMilliseconds` the request sent, the span it runs under)
        let rows: &[(&str, Option<i32>, ::std::time::Duration)] = &[
            ("no timeout at all", None, crate::state::DEFAULT_QUERY_TIMEOUT),
            (
                "a span inside the documented one",
                Some(1_500),
                ::std::time::Duration::from_millis(1_500),
            ),
            (
                "a span past the documented one",
                Some(i32::MAX),
                crate::state::DEFAULT_QUERY_TIMEOUT,
            ),
            ("no span at all", Some(0), crate::state::DEFAULT_QUERY_TIMEOUT),
            (
                "a span below zero",
                Some(-1),
                crate::state::DEFAULT_QUERY_TIMEOUT,
            ),
        ];
        for (case, requested, expected) in rows {
            let actual = query_run_timeout(*requested);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the text one request may have read is bounded, and a text past that bound is
    /// reported as the oversized query it is rather than parsed.
    #[test]
    fn an_oversized_query_text_is_not_parsed() {
        let data = GraphData::empty(::std::time::SystemTime::UNIX_EPOCH);
        let ceiling = crate::state::MAX_QUERY_STRING_CHARS;
        // (case, how many characters the text carries, whether it is read at all)
        let rows: &[(&str, usize, bool)] = &[
            ("at the bound", ceiling, true),
            ("one character past the bound", ceiling + 1, false),
            ("far past the bound", ceiling * 2, false),
        ];
        for (case, characters, read) in rows {
            // A text of the row's length that is a query as far as its length goes: the
            // row is about the LENGTH, so the text is padded with a comment the grammar
            // accepts.
            let query = format!("MATCH (n) RETURN n // {}", "x".repeat(characters - 22));
            assert_eq!(
                query.chars().count(),
                *characters,
                "case {case:?}: the row's text must carry {characters} characters"
            );
            match run_query(&data, &query, soon()) {
                Ok(_) => assert!(
                    *read,
                    "case {case:?}: a text past the bound must not be read"
                ),
                Err(QueryFailure::TooLong { characters: counted, ceiling: bound }) => {
                    assert!(
                        !*read,
                        "case {case:?}: a text at the bound must be read, got the \
                         oversized refusal"
                    );
                    assert_eq!(
                        (counted, bound),
                        (*characters, ceiling),
                        "case {case:?}: expected the refusal to count the text it read"
                    );
                }
                Err(other) => panic!("case {case:?}: unexpected failure {other:?}"),
            }
        }
    }

    /// RULE: a query that reads nothing of the graph projects ONE row, whatever the
    /// graph holds, and a text no reading accepts is refused at a position in the
    /// caller's own text.
    #[test]
    fn a_query_with_no_reading_clause_projects_one_row() {
        let empty = GraphData::empty(::std::time::SystemTime::UNIX_EPOCH);
        let data = two_cities();
        // (case, the query, the data it runs over, the rows it answers)
        let rows: &[(&str, &str, &GraphData, usize)] = &[
            ("a constant over an empty graph", "RETURN 1 AS one", &empty, 1),
            ("a constant over a graph holding data", "RETURN 1 AS one", &data, 1),
            (
                "several constants",
                "RETURN 'a' AS left, 2 AS right",
                &empty,
                1,
            ),
            (
                "a reading clause the caller wrote",
                "MATCH (c:city) RETURN c.name AS name",
                &data,
                2,
            ),
        ];
        for (case, query, over, expected) in rows {
            let parsed = parse_query(query)
                .unwrap_or_else(|refused| panic!("case {case:?}: expected a parse, got {refused}"));
            let answered = ::cypher_parser::execute(&within_the_usual_bounds(over), &parsed)
                .unwrap_or_else(|refused| panic!("case {case:?}: expected a result, got {refused}"));
            assert_eq!(
                answered.rows.len(),
                *expected,
                "case {case:?}: expected {expected} row(s), got {:?}",
                answered.rows
            );
        }
        let refused = parse_query("RETURN").expect_err("a projection of nothing is no query");
        let ::cypher_parser::CypherError::Syntax { position, .. } = refused else {
            panic!("case \"a projection of nothing\": expected a syntax refusal, got {refused}");
        };
        assert!(
            position <= "RETURN".len(),
            "case \"a projection of nothing\": expected a position in the caller's own \
             text, got {position}"
        );
    }

    /// RULE: a position the parser reports is worded as a line, a column and the offset
    /// it was reported at, so the same refusal reads the same for a query on one line
    /// and one spread over several.
    #[test]
    fn a_syntax_refusal_states_the_position_it_was_reported_at() {
        // (case, query, offset, the line and column it must report)
        let rows: &[(&str, &str, usize, usize, usize)] = &[
            ("the first character", "X", 0, 1, 1),
            ("mid-line", "MATCH (n RETURN n", 9, 1, 10),
            ("the second line", "MATCH (n)\nRETRN n", 10, 2, 1),
            ("past the end", "MATCH", 99, 1, 6),
        ];
        for (case, query, offset, line, column) in rows {
            let worded = syntax_refusal(query, "expected something else", *offset);
            let expected = format!("(line {line}, column {column}");
            assert!(
                worded.contains(&expected),
                "case {case:?}: expected {expected:?} in the refusal, got {worded:?}"
            );
        }
    }

    /// RULE: what a refusal lists as expected is the GRAMMAR's answer for that one
    /// position — the constructs a parse of the caller's own text can continue with
    /// there, and the productions that stand beside them — each named as the grammar
    /// names it, in the order the grammar lists them. The position inside an unclosed
    /// node pattern is the one every part of a node pattern is admitted at.
    #[test]
    fn the_alternatives_of_one_position_are_named_as_the_grammar_names_them() {
        // The alternatives of a node-pattern position, in order. Source: the parts of the
        // openCypher `NodePattern` production — the text a parse skips, the keyword a node
        // pattern's own continuation is spelled with, `NodeLabels`, `Properties` and its
        // `MapLiteral` form, the token that closes the pattern — plus the
        // `RelationshipPattern` the pattern may continue into once it is closed.
        let node_pattern_position = "whitespace, comment, COPY, node labels, a property \
                                     map, ')', MapLiteral or a relationship pattern";
        // (case, the text, the offset a parse of it stops at)
        let rows: &[(&str, &str, usize)] = &[
            ("an unclosed node pattern", "MATCH (n RETURN n", 9),
            // The same position in another text, reached another way: the defect is the
            // same one, so the grammar's answer is the same list.
            ("an unclosed node pattern of a longer query", "MATCH (a)-[:R]->(b LIMIT 1", 19),
        ];
        for (case, query, offset) in rows {
            let listed = expected_alternatives(query, *offset).unwrap_or_else(|| {
                panic!("case {case:?}: expected the grammar to admit something at {offset}")
            });
            assert_eq!(
                listed, node_pattern_position,
                "case {case:?}: expected the node pattern's own alternatives, got {listed:?}"
            );
        }
    }

    /// RULE: what a refusal lists as expected is the GRAMMAR's answer for that one
    /// position — the constructs a parse of the caller's own text can continue with
    /// there — so each listed alternative really can stand at that position, and
    /// positions the grammar reads differently are answered differently.
    #[test]
    fn the_alternatives_are_the_grammars_answer_at_one_position() {
        // (case, the text, the offset a parse of it stops at, a construct the grammar
        // admits at that offset)
        let rows: &[(&str, &str, usize, &str)] = &[
            ("an unclosed node pattern", "MATCH (n RETURN n", 9, "')'"),
            ("before any clause", "!!!", 0, "MATCH"),
            ("after a complete pattern", "MATCH (n) !", 10, "RETURN"),
            ("an unclosed list", "UNWIND [0 AS x RETURN x", 10, "']'"),
        ];
        for (case, query, offset, admitted) in rows {
            let listed = expected_alternatives(query, *offset).unwrap_or_else(|| {
                panic!("case {case:?}: expected the grammar to admit something at {offset}")
            });
            assert!(
                listed.split(", ").any(|named| named == *admitted)
                    || listed.ends_with(&format!(" or {admitted}"))
                    || listed == *admitted,
                "case {case:?}: expected {admitted:?} among the alternatives, got {listed:?}"
            );
        }
        // Two positions the grammar reads differently cannot be answered with one list:
        // inside an unclosed node pattern only the rest of that pattern may follow,
        // while before any clause only a clause may start.
        let inside = expected_alternatives("MATCH (n RETURN n", 9)
            .expect("the grammar admits the rest of a node pattern");
        let before = expected_alternatives("!!!", 0).expect("the grammar admits a clause");
        assert_ne!(
            inside, before,
            "expected the alternatives to differ between positions, got {inside:?} at both"
        );
    }

    /// RULE: every variable part of the refusal comes from the SUBMITTED text, so the
    /// same defect written at another position is reported at that position, with the
    /// same input and the same alternatives.
    #[test]
    fn the_same_defect_moved_through_the_text_is_reported_where_it_now_is() {
        let query = "MATCH (n RETURN n";
        let at = 9;
        let here = syntax_refusal(query, "expected something else", at);
        assert_eq!(
            here,
            format!(
                "Invalid input 'R': expected {} (line 1, column 10 (offset: 9))",
                expected_alternatives(query, at).expect("the grammar admits something there")
            ),
            "expected the refusal to state the input, the alternatives and the position"
        );
        // The same text after a line of its own: the input and the alternatives are the
        // same facts, the position is the one the text now has.
        let moved = format!("// a comment\n{query}");
        let shifted = at + "// a comment\n".len();
        let there = syntax_refusal(&moved, "expected something else", shifted);
        assert_eq!(
            there,
            format!(
                "Invalid input 'R': expected {} (line 2, column 10 (offset: {shifted}))",
                expected_alternatives(&moved, shifted)
                    .expect("the grammar admits something there")
            ),
            "expected the moved text to be reported at its own position, got {there:?}"
        );
        // A position is counted in CHARACTERS of the submitted text, so a multi-byte
        // character ahead of the defect moves it by ONE however many bytes it is
        // written with.
        let wide = format!("// é\n{query}");
        let wide_bytes = "// é\n".len() + at;
        let worded = syntax_refusal(&wide, "expected something else", wide_bytes);
        assert!(
            worded.ends_with("(line 2, column 10 (offset: 14))"),
            "expected the position to be counted in characters, got {worded:?}"
        );
    }

    /// RULE: a text stopped at its own end reports the end of the input rather than a
    /// character it does not have.
    #[test]
    fn a_text_that_stops_at_its_end_reports_the_end_of_the_input() {
        for (case, query, offset) in [
            ("the end of the text", "MATCH (n) RETURN", 16),
            ("past the end of the text", "MATCH", 99),
        ] {
            let worded = syntax_refusal(query, "expected something else", offset);
            assert!(
                worded.starts_with("Invalid input 'EOI': expected "),
                "case {case:?}: expected the end of the input to be named, got {worded:?}"
            );
        }
    }

    /// RULE: how deep a text nests is measured for EVERY text a caller sends, whether or
    /// not the lexer can read it, and no text past that bound is handed to a parser —
    /// neither to run it nor to find out what could have continued it.
    #[test]
    fn a_text_the_lexer_cannot_read_is_measured_for_depth_too() {
        let data = two_cities();
        let brackets = "(".repeat(MAX_QUERY_NESTING_DEPTH + 1);
        // (case, a text nesting one level past the bound) — the second carries a
        // character no query is written with after its brackets, which is the text the
        // engine's lexer may refuse outright.
        let rows: &[(&str, String)] = &[
            (
                "a text the lexer can read",
                format!("MATCH {brackets} RETURN 1 AS one"),
            ),
            ("a text the lexer may refuse", format!("{brackets}\u{0}")),
        ];
        for (case, query) in rows {
            assert!(
                nesting_depth(query) > MAX_QUERY_NESTING_DEPTH,
                "case {case:?}: expected the depth to be measured past the bound, got {}",
                nesting_depth(query)
            );
            let refused = run_query(&data, query, soon());
            assert!(
                matches!(refused, Err(QueryFailure::TooDeep { .. })),
                "case {case:?}: expected the text to be refused for its depth, got \
                 {refused:?}"
            );
            // And the wording of a refusal descends through no such text either: the
            // alternatives are left to the engine's own report instead.
            assert!(
                expected_alternatives(query, brackets.len()).is_none(),
                "case {case:?}: expected no parse of a text past the depth bound"
            );
        }
    }

    /// RULE: what an evaluation may be handed is charged for what it can EXPAND to, so a
    /// query chaining expansions over a list the GRAPH holds is stopped while it is
    /// building rows rather than after — a list a query reads is not measurable from its
    /// text, so the elements are charged as the graph hands them over.
    #[test]
    fn an_expansion_of_a_stored_list_is_charged_for_its_elements() {
        /// A graph holding one node whose `items` property is a list of `elements`
        /// numbers, written through the store's own writer.
        fn one_list(elements: usize) -> GraphData {
            let mut data = GraphData::empty(::std::time::SystemTime::UNIX_EPOCH);
            data.insert_node(
                "n1",
                GraphNode {
                    labels: BTreeSet::from([String::from("bag")]),
                    properties: BTreeMap::from([(
                        String::from("items"),
                        ::serde_json::Value::Array(
                            (0..elements)
                                .map(|item| ::serde_json::Value::from(item as i64))
                                .collect(),
                        ),
                    )]),
                },
                ::std::time::SystemTime::UNIX_EPOCH,
            )
            .expect("the node fits the store");
            data
        }

        // A short list is expanded and answered, which is what makes the row below a
        // statement about the BOUND rather than about the clause.
        let few = one_list(3);
        let answered = run_query(&few, "MATCH (n) UNWIND n.items AS item RETURN item", soon())
            .expect("a short stored list is expanded");
        let payload: ::serde_json::Value =
            ::serde_json::from_slice(&answered).expect("the payload is JSON");
        assert_eq!(
            payload[RESULTS_MEMBER].as_array().map(Vec::len),
            Some(3),
            "case \"a short stored list\": expected one row per element, got {payload}"
        );

        // The same text over a list long enough that chaining its expansions would build
        // more rows than one evaluation may be handed: the budget is spent on the
        // elements as they are handed over, so the evaluation is stopped.
        let many = one_list(100);
        let stopped = run_query(
            &many,
            "MATCH (n) UNWIND n.items AS a UNWIND n.items AS b UNWIND n.items AS c \
             RETURN a, b, c",
            soon(),
        );
        assert!(
            matches!(stopped, Err(QueryFailure::Stopped(Stopped::Exhausted))),
            "case \"a chained expansion of a stored list\": expected the evaluation to be \
             stopped while it was being handed the elements, got {stopped:?}"
        );
    }
}
