// SPDX-License-Identifier: CC0-1.0

//! The openCypher data plane: how a submitted query is read, what it is allowed to
//! spend, and how it reads and writes the stored graph.
//!
//! `ExecuteOpenCypherQuery` is the operation that runs a caller-supplied query, and it
//! runs it for real:
//!
//! * the text is LEXED and PARSED by the `opencypher` crate's openCypher parser, so a
//!   malformed query is refused by a real parser at the position it could not be read,
//!   and a well-formed one reaches this module as the crate's typed AST;
//! * the AST is EVALUATED here, against [`super::PropertyGraph`] — the same nodes and
//!   relationships every other reading of this engine's data answers from. There is no
//!   second copy of the graph and nothing in a result is synthesised: a row carrying an
//!   element reports what the store holds for it.
//!
//! What the parser decides and what this module decides are kept apart: whether the text
//! IS a query is the parser's (it owns the grammar and the position a refusal names), and
//! what the query DOES to this engine's graph is this module's.
//!
//! ## What one query may spend
//!
//! A query is a caller-chosen input, so every bound on the work one costs is stated here
//! and taken BEFORE the work it caps:
//!
//! * the text one request may carry ([`MAX_QUERY_CHARS`]) and how deep it may nest
//!   ([`MAX_QUERY_NESTING_DEPTH`]), both checked before the parser sees it, and the
//!   `parameters` member it may carry ([`MAX_PARAMETERS_CHARS`]), which bounds every
//!   value a parameter binds;
//! * the candidate elements and evaluation steps one run may be handed
//!   ([`MAX_QUERY_WORK_UNITS`]) and the span it may spend being handed them
//!   ([`QUERY_RUN_TIMEOUT`]), which together stop a query whose cost diverges;
//! * how large one value may be ([`MAX_VALUE_CHARS`], [`MAX_LIST_ITEMS`]) and how deep
//!   the evaluation may descend ([`MAX_EVALUATION_DEPTH`]), with every value charged for
//!   its SIZE ([`Budget::charge_size`]) before it is built, so a query that doubles what
//!   it allocates per step spends the budget as fast as it grows the heap;
//! * how far a variable-length pattern may walk ([`MAX_VARIABLE_LENGTH_HOPS`]), the rows
//!   a result may carry ([`MAX_QUERY_RESULT_ROWS`]), the characters it may render to
//!   ([`MAX_RESULT_CHARS`]) and the program a `=~` pattern may compile to
//!   ([`MAX_REGEX_PROGRAM_BYTES`]);
//! * the property data the store keeps ([`MAX_STORE_BYTES`]), beside the counts of
//!   elements it holds ([`super::MAX_NODES`], [`super::MAX_RELATIONSHIPS`]), so a
//!   sequence of requests cannot grow the store without end;
//! * the stack one run is given ([`QUERY_STACK_BYTES`]): parsing and evaluating descend
//!   through a nested query, so the run happens on a thread sized for the nesting this
//!   engine admits rather than on whatever stack the request arrived on.
//!
//! A query past a bound is REFUSED with the limit it met. None is narrowed and answered
//! anyway: a short result reported as a complete one is worse than a refusal.
//!
//! ## Atomicity
//!
//! A query that writes runs against a COPY of the graph, which replaces the stored one
//! only once the whole query has succeeded ([`run_query`]). So a query that fails
//! half-way leaves the graph as it was, and a caller that reads after a write that
//! reported success sees all of it — the store's own lock is what serialises two writers,
//! so concurrent writes are applied one whole query at a time.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ::opencypher::{
    Clause, Create, Delete, Executor, Expr, Match, Merge, PatternPart, Query, RemoveItem, Return,
    ReturnBody, SetItem, SingleQuery, Unwind, Visitor,
};
use ::serde_json::Value;

use crate::ops::types::{
    ConstraintViolationException, FailureByQueryException, InvalidParameterException,
    MalformedQueryException, MemoryLimitExceededException, QueryLimitException,
    QueryTooLargeException, TimeLimitExceededException, UnsupportedOperationException,
};

use super::property_graph::{GraphWriteError, PropertyGraph};

mod plan;
mod request;
mod values;

pub use plan::{
    explain_query, BranchRun, ExplainMode, RunTrace, StepRun, OPEN_CYPHER_EXPLAIN_MODES,
};
pub use request::{
    query_outcome, query_refusal, request_parameters, QueryRefusal, OPEN_CYPHER_QUERY_MEMBER,
    PARAMETERS_MEMBER,
};
pub use values::{is_aggregate, CypherValue, Row};

use values::{created_labels, group_key, order, property_json, row_bytes, truth, Reader};

/// The longest openCypher query text this engine reads, in characters.
///
/// Emulator resource guard, NOT a quota AWS publishes: parsing and evaluating both
/// descend through the text, so this is the bound that makes the deepest descent either
/// can take a known quantity (see [`QUERY_STACK_BYTES`]). A request past it is refused as
/// too large rather than read.
pub const MAX_QUERY_CHARS: usize = 8_192;

/// The deepest a query text may nest before it is read.
///
/// Emulator resource guard: the parser descends through a nested expression, so a text
/// nesting arbitrarily deep would recurse arbitrarily deep on a caller's input. The depth
/// is counted off the text's own brackets ([`nesting_depth`]) BEFORE the parser sees it.
pub const MAX_QUERY_NESTING_DEPTH: usize = 64;

/// The most work units one run may spend.
///
/// Emulator resource guard, charged in candidate elements and evaluation steps: every
/// node a pattern is handed, every relationship a hop scans and every expression
/// evaluated costs one, so a query whose cost grows with the graph or with its own
/// pattern is stopped as it is doing the work rather than after.
pub const MAX_QUERY_WORK_UNITS: usize = 2_000_000;

/// The most rows one result may carry.
///
/// Emulator resource guard: a pattern naming several elements answers a row per
/// COMBINATION of them, so this is the last bound on what one request can be answered
/// with. A result past it is reported as the limit it met rather than truncated to look
/// complete.
pub const MAX_QUERY_RESULT_ROWS: usize = 10_000;

/// How long one run may spend being handed the graph.
///
/// Emulator resource guard: the deadline is read before every charge, so a query whose
/// cost diverges stops asking at the deadline rather than running on past it. It is also
/// the longest one request can occupy the task that serves it, since the run holds the
/// store's lock — so at most one query is being evaluated at a time, however many
/// requests arrive.
pub const QUERY_RUN_TIMEOUT: Duration = Duration::from_secs(5);

/// The most hops a variable-length pattern (`-[*]->`) may walk.
///
/// Emulator resource guard: an unbounded `*` walks every path in the graph, and the
/// number of paths grows faster than the graph does. A pattern asking to walk FURTHER is
/// refused with this ceiling rather than evaluated over a narrowed range, so a short
/// answer is never reported as a complete one.
pub const MAX_VARIABLE_LENGTH_HOPS: usize = 8;

/// The most characters one value an evaluation produces may carry, and the most items one
/// list may hold.
///
/// Emulator resource guards, and the ones that bound MEMORY rather than steps: `+` on two
/// strings or two lists produces a value as large as both operands together, so a query
/// chaining such operations doubles what it allocates per step. The size is checked, and
/// charged against the budget in proportion to it ([`Budget::charge_size`]), BEFORE the
/// value is built.
pub const MAX_VALUE_CHARS: usize = 1 << 20;
pub const MAX_LIST_ITEMS: usize = 100_000;

/// How many characters of a value one work unit pays for.
///
/// What makes [`MAX_QUERY_WORK_UNITS`] bound the bytes an evaluation allocates as well as
/// the steps it takes: an operation that produces a large value, and every copy of a row
/// the pipeline makes, is charged for its size, so a chain of doublings spends the budget
/// as fast as it grows the heap.
pub const BUDGET_CHARS_PER_UNIT: usize = 64;

/// The most characters of ROWS one run may copy.
///
/// Emulator resource guard on what one run can hold live: the rows a clause answers are
/// built by copying the rows before them, so a value bound once cheaply would otherwise
/// be duplicated into every row of a large result. Every copy is charged against this
/// ceiling as well as against the work budget, and a run is never holding more than it
/// has copied.
pub const MAX_COPIED_ROW_CHARS: usize = 32 << 20;

/// The most characters one result may render to.
///
/// Emulator resource guard on the AMPLIFICATION a small request may answer with: the rows
/// are capped ([`MAX_QUERY_RESULT_ROWS`]) and each value in them is capped
/// ([`MAX_VALUE_CHARS`]), and this is the bound on their product.
pub const MAX_RESULT_CHARS: usize = 8 << 20;

/// The deepest an evaluation may descend.
///
/// Emulator resource guard: evaluating a nested expression, walking the hops of a pattern
/// and rendering a nested value each descend through the query's own structure, so the
/// depth is counted ([`Budget::enter`]) and a run past this ceiling is refused — an
/// overflowing stack ends the process, which is not a failure one caller's query may
/// cause.
pub const MAX_EVALUATION_DEPTH: usize = 128;

/// The most characters the request's `parameters` member may carry.
///
/// Emulator resource guard: every value a parameter binds is read out of this text, so
/// bounding the text bounds the values derived from it.
pub const MAX_PARAMETERS_CHARS: usize = 256 * 1024;

/// The most bytes of property data one store holds.
///
/// Emulator resource guard beside [`super::MAX_NODES`] and
/// [`super::MAX_RELATIONSHIPS`], which cap the COUNT of elements and not their size: a
/// sequence of requests that each write a larger value to the same element would
/// otherwise grow the store without end.
pub const MAX_STORE_BYTES: usize = 64 << 20;

/// The most bytes a regular expression written in a query may compile to.
///
/// Emulator resource guard: `=~` takes its pattern from the query, so the compiled
/// program is bounded before it is built.
pub const MAX_REGEX_PROGRAM_BYTES: usize = 1 << 20;

/// The stack one run is given.
///
/// The parser and the evaluation both descend through a nested query, so the thread that
/// runs them is given a stack sized for the text this engine admits ([`MAX_QUERY_CHARS`])
/// with room to spare, rather than left on whatever the request arrived on: an
/// overflowing stack ends the process, which is not a failure one caller's query may
/// cause.
pub const QUERY_STACK_BYTES: usize = 32 * 1024 * 1024;

/// Why a query this data plane was handed could not be answered.
///
/// Each variant is a different fact about the REQUEST, which is why they are kept apart:
/// the operation reports each one through the modelled error that states it.
#[derive(Debug)]
pub enum QueryFailure {
    /// The text could not be read as openCypher, at the position the parser names.
    Malformed { reason: String },
    /// The query names a clause this engine does not run.
    Unsupported { clause: &'static str },
    /// The query is well formed but could not be evaluated: a type it does not hold, a
    /// function this engine does not implement, a variable it never bound.
    Refused(String),
    /// The query asked for something the graph's own rules do not allow.
    Constraint(String),
    /// The query references a parameter the request did not carry.
    MissingParameter(String),
    /// The query text is longer than this engine reads.
    TooLong { characters: usize, ceiling: usize },
    /// The query text nests deeper than this engine reads.
    TooDeep { depth: usize, ceiling: usize },
    /// The result is larger than this engine answers with.
    TooLarge { rows: usize, ceiling: usize },
    /// One value, or one rendered result, is larger than this engine builds.
    ValueTooLarge { size: usize, ceiling: usize },
    /// The evaluation descended deeper than this engine evaluates.
    TooNested { ceiling: usize },
    /// A variable-length pattern asked to walk further than this engine walks.
    TooManyHops { asked: usize, ceiling: usize },
    /// The run asked for more work than one query may spend.
    Exhausted { ceiling: usize },
    /// The span the run was given ran out while it was still working.
    TimedOut { ceiling: Duration },
    /// The graph could not take a write: it is full, or the write named an element that
    /// is not there.
    Write(GraphWriteError),
    /// The run did not finish: this engine could not start it, or it ended without an
    /// answer. A server-side failure, not anything the caller sent.
    Interrupted(String),
}

/// What one run may still spend, how deep it may descend, and when it must stop.
///
/// Every charge is taken BEFORE the work it pays for, so nothing past a ceiling is
/// computed, and the deadline is read on every charge, so a run that is still asking when
/// its span ends is stopped rather than left running after its caller has been answered.
#[derive(Debug)]
pub struct Budget {
    /// The work units this run may still spend.
    left: Cell<usize>,
    /// How many units it was given, which is the ceiling a refusal reports.
    units: usize,
    /// The instant after which it may spend none.
    deadline: Instant,
    /// The span it was given, which is the ceiling a refusal reports.
    span: Duration,
    /// How deep the evaluation is right now.
    depth: Cell<usize>,
    /// The deepest it may descend.
    ceiling: usize,
    /// How many characters of rows the run has copied.
    copied: Cell<usize>,
}

/// One level of the evaluation's descent, given back when the level is left.
///
/// Held for as long as the recursive call it belongs to, so the depth counted is the
/// depth of the call stack rather than a count a missed decrement could leak.
#[derive(Debug)]
pub struct Descent<'budget> {
    budget: &'budget Budget,
}

impl Drop for Descent<'_> {
    fn drop(&mut self) {
        let depth = self.budget.depth.get();
        self.budget.depth.set(depth.saturating_sub(1));
    }
}

impl Budget {
    /// A budget of `units` work units, descending at most [`MAX_EVALUATION_DEPTH`],
    /// ending at `deadline`.
    #[must_use]
    pub fn of(units: usize, deadline: Instant) -> Self {
        Self {
            left: Cell::new(units),
            units,
            deadline,
            span: deadline.saturating_duration_since(Instant::now()),
            depth: Cell::new(0),
            ceiling: MAX_EVALUATION_DEPTH,
            copied: Cell::new(0),
        }
    }

    /// Charge `units`, or report the ceiling that stopped the run.
    pub fn charge(&self, units: usize) -> Result<(), QueryFailure> {
        if Instant::now() >= self.deadline {
            return Err(QueryFailure::TimedOut { ceiling: self.span });
        }
        let left = self.left.get();
        if units > left {
            return Err(QueryFailure::Exhausted {
                ceiling: self.units,
            });
        }
        self.left.set(left - units);
        Ok(())
    }

    /// Charge for a value of `characters` characters, and refuse one larger than this
    /// engine builds.
    ///
    /// This is what bounds the MEMORY an evaluation may take: the charge is proportional
    /// to the size of the value the caller is about to build
    /// ([`BUDGET_CHARS_PER_UNIT`]), and the ceiling is the size of one value
    /// ([`MAX_VALUE_CHARS`]). Taken before the value is built.
    pub fn charge_size(&self, characters: usize, ceiling: usize) -> Result<(), QueryFailure> {
        if characters > ceiling {
            return Err(QueryFailure::ValueTooLarge {
                size: characters,
                ceiling,
            });
        }
        self.charge(characters / BUDGET_CHARS_PER_UNIT + 1)
    }

    /// Charge for ONE more item of a value being built item by item, where `held` is how
    /// many it already holds.
    ///
    /// The same ceiling as [`Self::charge_size`] and the same charge per item, without
    /// re-charging for the items already there: a value built in a loop pays once per
    /// item rather than once per item per item.
    pub fn charge_item(&self, held: usize, ceiling: usize) -> Result<(), QueryFailure> {
        if held >= ceiling {
            return Err(QueryFailure::ValueTooLarge {
                size: held.saturating_add(1),
                ceiling,
            });
        }
        self.charge(1)
    }

    /// Descend one level, or refuse a run that is already as deep as this engine
    /// evaluates. The level is given back when the returned value is dropped.
    pub fn enter(&self) -> Result<Descent<'_>, QueryFailure> {
        let depth = self.depth.get();
        if depth >= self.ceiling {
            return Err(QueryFailure::TooNested {
                ceiling: self.ceiling,
            });
        }
        self.depth.set(depth.saturating_add(1));
        Ok(Descent { budget: self })
    }

    /// Charge for copying a row of `characters` characters, and refuse a run that has
    /// copied more than it may.
    ///
    /// This is what bounds what one run HOLDS: a clause answers its rows by copying the
    /// rows before it, so the bytes of every copy are charged in proportion
    /// ([`BUDGET_CHARS_PER_UNIT`]) and counted against [`MAX_COPIED_ROW_CHARS`]. A run
    /// never holds more rows than it has copied, so the ceiling on what it copies is a
    /// ceiling on what it holds.
    pub fn charge_copy(&self, characters: usize) -> Result<(), QueryFailure> {
        let copied = self.copied.get().saturating_add(characters);
        if copied > MAX_COPIED_ROW_CHARS {
            return Err(QueryFailure::ValueTooLarge {
                size: copied,
                ceiling: MAX_COPIED_ROW_CHARS,
            });
        }
        self.copied.set(copied);
        self.charge(characters / BUDGET_CHARS_PER_UNIT + 1)
    }
}

/// How deep `query`'s brackets nest, and how deep a run of prefix operators written in
/// front of a value goes.
///
/// Not a parse — the `opencypher` crate owns that — but the one measurement that has to
/// be taken BEFORE one: how far a parser reading this text would descend. A bracket
/// inside a string literal, a quoted name or a comment is not a bracket of the query, so
/// quoted text and both comment forms are skipped, which is what keeps the measurement
/// from refusing a query whose DATA happens to contain brackets. A chain of prefix
/// operators (`NOT NOT …`, `- - …`) nests a parser just as a bracket does without writing
/// one, so a run of them counts too.
///
/// It FAILS CLOSED: a text whose quote or comment never closes is one this measurement
/// and the parser's own lexer could read differently, so the text it could not read as
/// quoted is measured as ordinary text instead of skipped — a bracket inside it is then
/// counted rather than lost.
#[must_use]
pub fn nesting_depth(query: &str) -> usize {
    let characters: Vec<char> = query.chars().collect();
    let mut at: usize = 0;
    let mut depth: usize = 0;
    let mut deepest: usize = 0;
    // A run of prefix operators, which nests a parse without opening a bracket.
    let mut prefixes: usize = 0;
    let next = |at: usize| characters.get(at).copied();
    while let Some(character) = next(at) {
        at = at.saturating_add(1);
        match character {
            '\'' | '"' => {
                // A string literal, to the matching quote; a backslash escapes the next
                // character.
                let mut scan = at;
                let mut closed = false;
                while let Some(inside) = next(scan) {
                    scan = scan.saturating_add(1);
                    if inside == '\\' {
                        scan = scan.saturating_add(1);
                    } else if inside == character {
                        closed = true;
                        break;
                    }
                }
                if closed {
                    at = scan;
                }
                prefixes = 0;
            }
            '`' => {
                // A quoted name, to the matching backtick. openCypher escapes a backtick
                // inside one by DOUBLING it, and a backslash is not an escape there.
                let mut scan = at;
                let mut closed = false;
                while let Some(inside) = next(scan) {
                    scan = scan.saturating_add(1);
                    if inside == '`' {
                        if next(scan) == Some('`') {
                            scan = scan.saturating_add(1);
                        } else {
                            closed = true;
                            break;
                        }
                    }
                }
                if closed {
                    at = scan;
                }
                prefixes = 0;
            }
            '/' if next(at) == Some('/') => {
                while let Some(inside) = next(at) {
                    at = at.saturating_add(1);
                    if inside == '\n' {
                        break;
                    }
                }
                prefixes = 0;
            }
            '/' if next(at) == Some('*') => {
                let mut scan = at;
                let mut previous = ' ';
                let mut closed = false;
                while let Some(inside) = next(scan) {
                    scan = scan.saturating_add(1);
                    if previous == '*' && inside == '/' {
                        closed = true;
                        break;
                    }
                    previous = inside;
                }
                if closed {
                    at = scan;
                }
                prefixes = 0;
            }
            '(' | '[' | '{' => {
                depth = depth.saturating_add(1);
                deepest = deepest.max(depth);
                prefixes = 0;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                prefixes = 0;
            }
            '!' | '-' | '+' => {
                prefixes = prefixes.saturating_add(1);
                deepest = deepest.max(depth.saturating_add(prefixes));
            }
            // Whitespace separates a run of prefix operators without ending it.
            character if character.is_whitespace() => {}
            character if character.is_alphabetic() || character == '_' => {
                let from = at.saturating_sub(1);
                while next(at).is_some_and(|inside| inside.is_alphanumeric() || inside == '_') {
                    at = at.saturating_add(1);
                }
                let word: String = characters
                    .get(from..at)
                    .unwrap_or_default()
                    .iter()
                    .collect();
                // `NOT` is the word form of a prefix operator, so a run of them nests a
                // parse the same way a run of `-` does.
                if word.eq_ignore_ascii_case("NOT") {
                    prefixes = prefixes.saturating_add(1);
                    deepest = deepest.max(depth.saturating_add(prefixes));
                } else {
                    prefixes = 0;
                }
            }
            _ => prefixes = 0,
        }
    }
    deepest
}

/// The rows `query` answers with, as this service spells them on the wire, after applying
/// whatever `query` writes to `graph`.
///
/// `parameters` is what the request's `parameters` member carried, by name. `deadline` is
/// the instant the run must stop at.
///
/// A query that WRITES is applied to a copy of the graph and replaces the stored one only
/// once the whole query has succeeded, so a query that fails part-way changes nothing.
/// Parsing and evaluation happen on a thread of this engine's own ([`QUERY_STACK_BYTES`]),
/// so a deeply nested query cannot overflow the stack the request arrived on, and a run
/// that ends without an answer is reported as a server-side failure rather than taking
/// the process with it.
pub fn run_query(
    graph: &mut PropertyGraph,
    source: &str,
    parameters: BTreeMap<String, Value>,
    deadline: Instant,
) -> Result<Vec<Value>, QueryFailure> {
    let characters = source.chars().count();
    if characters > MAX_QUERY_CHARS {
        return Err(QueryFailure::TooLong {
            characters,
            ceiling: MAX_QUERY_CHARS,
        });
    }
    let depth = nesting_depth(source);
    if depth > MAX_QUERY_NESTING_DEPTH {
        return Err(QueryFailure::TooDeep {
            depth,
            ceiling: MAX_QUERY_NESTING_DEPTH,
        });
    }

    let mut working = PropertyGraph::default();
    let answered = ::std::thread::scope(|scope| {
        // ONE thread per run, and a run only starts once its caller holds the store's
        // lock, so this engine never has more than one evaluation in flight. The thread
        // is what gives the run a stack of its own, and what keeps a run that ends
        // without an answer from taking the process with it: the panic is reported as a
        // server-side failure to the one caller whose query caused it.
        let run = ::std::thread::Builder::new()
            .stack_size(QUERY_STACK_BYTES)
            .spawn_scoped(scope, || {
                let parsed = ::opencypher::parse(source).map_err(|refused| {
                    // The refusal is worded the way the openCypher endpoint words one,
                    // from the parser's own report: what it could not read, what it
                    // expected there, and where in THIS query it stopped.
                    QueryFailure::Malformed {
                        reason: super::open_cypher_parse_failure(
                            source,
                            &refused.message,
                            refused.span.start..refused.span.end,
                        ),
                    }
                })?;
                // Semantic checks a query is refused by BEFORE it is evaluated, so what
                // the engine answers does not depend on whether the graph happens to
                // hold a row the clause in question would have been evaluated for.
                validated(&parsed, &parameters)?;
                let writes = writes_the_graph(&parsed);
                if writes {
                    // The copy is what makes the whole query atomic: it is written to,
                    // and only a query that finished replaces the stored graph.
                    working = graph.clone();
                }
                let target: &mut PropertyGraph = if writes { &mut working } else { &mut *graph };
                let mut run = QueryRun {
                    graph: target,
                    source,
                    parameters,
                    budget: Budget::of(MAX_QUERY_WORK_UNITS, deadline),
                    // A query answered through the query endpoint is run UNTRACED: the
                    // per-operator measurements only an explain reports are only taken
                    // when one is asked for.
                    trace: None,
                };
                let result = run.execute(&parsed)?;
                let rendered = run.rendered(&result)?;
                Ok((writes, rendered))
            });
        match run {
            Ok(handle) => handle.join().unwrap_or_else(|_| {
                Err(QueryFailure::Interrupted(
                    "the openCypher engine did not finish this query".to_string(),
                ))
            }),
            Err(refused) => Err(QueryFailure::Interrupted(format!(
                "the openCypher engine could not start a run: {refused}"
            ))),
        }
    });
    let (writes, rendered) = answered?;
    if writes {
        *graph = working;
    }
    Ok(rendered)
}

/// Refuse `query` for what it SAYS, before any of it is evaluated.
///
/// Three things are decided from the query alone, so a query carrying any of them is
/// refused the same way whatever the graph holds: every parameter it references must be
/// one the request carried, every function it calls must be one this engine implements,
/// and every variable it reads must be one the query binds somewhere. An engine that only
/// noticed these while evaluating would answer a query over an empty graph and refuse the
/// same query over a graph with a row in it.
fn validated(query: &Query, parameters: &BTreeMap<String, Value>) -> Result<(), QueryFailure> {
    /// Collects the first thing the query says that cannot be served, and the names it
    /// binds and reads.
    struct Checked<'request> {
        parameters: &'request BTreeMap<String, Value>,
        refused: Option<QueryFailure>,
        /// Every variable the query binds anywhere.
        bound: Vec<String>,
        /// Every variable the query reads.
        read: Vec<String>,
    }
    impl Checked<'_> {
        /// A parameter the query references must be one the request carried.
        fn parameter(&mut self, name: &str) {
            if self.refused.is_none() && !self.parameters.contains_key(name) {
                self.refused = Some(QueryFailure::MissingParameter(name.to_string()));
            }
        }

        fn binds(&mut self, variable: Option<&::opencypher::Variable>) {
            if let Some(variable) = variable {
                self.bound.push(variable.0.clone());
            }
        }
    }
    impl Visitor for Checked<'_> {
        fn visit_expr(&mut self, expr: &Expr) {
            match &expr.kind {
                ::opencypher::ExprKind::Parameter(name) => self.parameter(name),
                ::opencypher::ExprKind::Variable(variable) => self.read.push(variable.0.clone()),
                ::opencypher::ExprKind::FunctionCall { name, .. } => {
                    if self.refused.is_none() && !values::is_implemented(name.0.as_str()) {
                        self.refused =
                            Some(QueryFailure::Refused(format!("Unknown function {name}()")));
                    }
                }
                // A comprehension or a quantifier binds its own element variable for the
                // expression inside it.
                ::opencypher::ExprKind::ListComprehension { variable, .. }
                | ::opencypher::ExprKind::Quantifier { variable, .. } => {
                    self.bound.push(variable.0.clone());
                }
                ::opencypher::ExprKind::PatternComprehension { variable, .. } => {
                    self.binds(variable.as_ref());
                }
                _ => {}
            }
            ::opencypher::walk_expr(self, expr);
        }

        fn visit_clause(&mut self, clause: &Clause) {
            match clause {
                Clause::Unwind(unwind) => self.bound.push(unwind.variable.0.clone()),
                Clause::With(with) => {
                    for item in &with.body.items {
                        self.binds(item.alias.as_ref());
                    }
                }
                Clause::Return(returning) => {
                    for item in &returning.body.items {
                        self.binds(item.alias.as_ref());
                    }
                }
                Clause::Foreach(foreach) => self.bound.push(foreach.variable.0.clone()),
                Clause::LoadCsv(load) => self.bound.push(load.variable.0.clone()),
                Clause::Set(set) => {
                    for item in &set.items {
                        match item {
                            SetItem::Replace { variable, .. }
                            | SetItem::Mutate { variable, .. }
                            | SetItem::Labels { variable, .. } => {
                                self.read.push(variable.0.clone());
                            }
                            _ => {}
                        }
                    }
                }
                Clause::Remove(remove) => {
                    for item in &remove.items {
                        if let RemoveItem::Labels { variable, .. } = item {
                            self.read.push(variable.0.clone());
                        }
                    }
                }
                Clause::Call(call) => {
                    if let ::opencypher::CallKind::Procedure(procedure) = &call.kind {
                        if let Some(yields) = &procedure.yields {
                            for item in &yields.items {
                                self.bound.push(item.field.0.clone());
                                self.binds(item.alias.as_ref());
                            }
                        }
                    }
                }
                _ => {}
            }
            ::opencypher::walk_clause(self, clause);
        }

        fn visit_pattern_part(&mut self, part: &::opencypher::PatternPart) {
            self.binds(part.variable.as_ref());
            ::opencypher::walk_pattern_part(self, part);
        }

        fn visit_node_pattern(&mut self, node: &::opencypher::NodePattern) {
            self.binds(node.variable.as_ref());
            if let Some(::opencypher::Properties::Param(name)) = &node.properties {
                self.parameter(name);
            }
            ::opencypher::walk_node_pattern(self, node);
        }

        fn visit_relationship_pattern(&mut self, relationship: &::opencypher::RelationshipPattern) {
            self.binds(relationship.variable.as_ref());
            if let Some(::opencypher::Properties::Param(name)) = &relationship.properties {
                self.parameter(name);
            }
            ::opencypher::walk_relationship_pattern(self, relationship);
        }
    }
    let mut checked = Checked {
        parameters,
        refused: None,
        bound: Vec::new(),
        read: Vec::new(),
    };
    checked.visit_query(query);
    if let Some(refused) = checked.refused {
        return Err(refused);
    }
    // A name the query reads and never binds cannot be evaluated against any graph.
    if let Some(unbound) = checked
        .read
        .iter()
        .find(|name| !checked.bound.iter().any(|held| held == *name))
    {
        return Err(QueryFailure::Refused(format!(
            "Variable `{unbound}` not defined"
        )));
    }
    Ok(())
}

/// Whether `query` carries a clause that CHANGES the graph.
fn writes_the_graph(query: &Query) -> bool {
    /// Looks for a writing clause anywhere in the query, nested clauses included.
    struct Writes(bool);
    impl Visitor for Writes {
        fn visit_clause(&mut self, clause: &Clause) {
            if matches!(
                clause,
                Clause::Create(_)
                    | Clause::Merge(_)
                    | Clause::Set(_)
                    | Clause::Delete(_)
                    | Clause::Remove(_)
                    | Clause::Foreach(_)
                    | Clause::LoadCsv(_)
            ) {
                self.0 = true;
            }
            ::opencypher::walk_clause(self, clause);
        }
    }
    let mut writes = Writes(false);
    writes.visit_query(query);
    writes.0
}

/// One result set: the columns the query projected, and a row per answer.
#[derive(Debug, Default)]
struct ResultSet {
    columns: Vec<String>,
    rows: Vec<Row>,
}

/// One run of one query over one graph.
struct QueryRun<'run> {
    /// The graph the run reads, and writes when the query says so.
    graph: &'run mut PropertyGraph,
    /// The query as it was submitted, which is where a projected column with no alias
    /// takes its name from.
    source: &'run str,
    /// What the request's parameters bound, by name.
    parameters: BTreeMap<String, Value>,
    /// What this run may still spend.
    budget: Budget,
    /// What each operator of the pipeline did, for a run that is being traced — which is
    /// what an explain in a measuring mode reports from. `None` for an untraced run, and
    /// nothing in the evaluation reads it: it is written to and never branched on, so a
    /// traced run answers exactly what an untraced one answers.
    trace: Option<RunTrace>,
}

/// What one projected item answers: a variable already in scope (`RETURN *`), or an
/// expression the query wrote.
enum Projected<'query> {
    Bound(String),
    Computed {
        name: String,
        expr: &'query Expr,
    },
}

impl Executor for QueryRun<'_> {
    type Output = ResultSet;
    type Error = QueryFailure;

    fn execute(&mut self, query: &Query) -> Result<Self::Output, Self::Error> {
        let mut answered = self.run_single(&query.initial)?;
        for union in &query.unions {
            let more = self.run_single(&union.query)?;
            if more.columns != answered.columns {
                return Err(QueryFailure::Refused(
                    "All sub queries in an UNION must have the same column names".to_string(),
                ));
            }
            let units_in = answered.rows.len().saturating_add(more.rows.len());
            let started = ::std::time::Instant::now();
            answered.rows.extend(more.rows);
            if !union.all {
                answered.rows = self.distinct(&answered.columns, answered.rows);
            }
            let elapsed = started.elapsed();
            let units_out = answered.rows.len();
            if let Some(trace) = &mut self.trace {
                trace.merge(StepRun {
                    units_in,
                    units_out,
                    elapsed,
                });
            }
            self.check_rows(answered.rows.len())?;
        }
        Ok(answered)
    }
}

impl QueryRun<'_> {
    /// The reader every evaluation goes through: the graph as it stands, the request's
    /// parameters and this run's budget, with no way to write.
    fn reader(&self) -> Reader<'_> {
        Reader {
            graph: &*self.graph,
            parameters: &self.parameters,
            budget: &self.budget,
        }
    }

    /// Run one single query (a clause sequence with no top-level `UNION`).
    ///
    /// The clauses are a pipeline over rows: it starts with ONE row that binds nothing —
    /// which is what makes a query with no reading clause answer one row — and each
    /// clause reads the rows before it and answers the rows after it.
    fn run_single(&mut self, single: &SingleQuery) -> Result<ResultSet, QueryFailure> {
        let injected = Instant::now();
        let mut rows: Vec<Row> = vec![Row::new()];
        if let Some(trace) = &mut self.trace {
            trace.begin_branch(StepRun {
                units_in: 0,
                units_out: rows.len(),
                elapsed: injected.elapsed(),
            });
        }
        let mut answered: Option<ResultSet> = None;
        for clause in &single.clauses {
            self.budget.charge(1)?;
            if answered.is_some() {
                return Err(QueryFailure::Refused(
                    "RETURN can only be used at the end of a query".to_string(),
                ));
            }
            let units_in = rows.len();
            let started = Instant::now();
            rows = match clause {
                Clause::Match(matching) => self.run_match(matching, rows)?,
                Clause::Unwind(unwind) => self.run_unwind(unwind, rows)?,
                Clause::With(with) => {
                    let (_, projected) = self.project(
                        &with.body,
                        with.distinct,
                        &rows,
                        with.where_clause.as_ref(),
                    )?;
                    projected
                }
                Clause::Create(create) => self.run_create(create, rows)?,
                Clause::Merge(merge) => self.run_merge(merge, rows)?,
                Clause::Set(set) => {
                    self.apply_set(&set.items, &rows)?;
                    rows
                }
                Clause::Remove(remove) => {
                    self.apply_remove(&remove.items, &rows)?;
                    rows
                }
                Clause::Delete(delete) => {
                    self.apply_delete(delete, &rows)?;
                    rows
                }
                Clause::Return(returning) => {
                    answered = Some(self.returned(returning, &rows)?);
                    Vec::new()
                }
                Clause::Call(_) => return Err(QueryFailure::Unsupported { clause: "CALL" }),
                Clause::Foreach(_) => {
                    return Err(QueryFailure::Unsupported { clause: "FOREACH" })
                }
                Clause::LoadCsv(_) => {
                    return Err(QueryFailure::Unsupported { clause: "LOAD CSV" })
                }
                _ => {
                    return Err(QueryFailure::Unsupported {
                        clause: "this clause",
                    })
                }
            };
            let elapsed = started.elapsed();
            if let Some(trace) = &mut self.trace {
                // A `RETURN` answers the result set rather than more rows, so what it
                // answered is counted off that set.
                let units_out = answered
                    .as_ref()
                    .map_or(rows.len(), |result| result.rows.len());
                trace.clause(StepRun {
                    units_in,
                    units_out,
                    elapsed,
                });
            }
            self.check_rows(rows.len())?;
        }
        // A query with no `RETURN` answers no rows, which is what a write-only query
        // answers: the writes happened and there is nothing to project.
        Ok(answered.unwrap_or_default())
    }

    /// The result set a `RETURN` answers.
    fn returned(&mut self, returning: &Return, rows: &[Row]) -> Result<ResultSet, QueryFailure> {
        let (columns, projected) = self.project(&returning.body, returning.distinct, rows, None)?;
        Ok(ResultSet {
            columns,
            rows: projected,
        })
    }

    // -------------------------------------------------------------- reading clauses

    /// `MATCH` / `OPTIONAL MATCH`: every way the patterns match, per incoming row.
    fn run_match(&mut self, matching: &Match, rows: Vec<Row>) -> Result<Vec<Row>, QueryFailure> {
        let mut out = Vec::new();
        for row in rows {
            let matched = {
                let reader = self.reader();
                reader.match_patterns(
                    &matching.patterns,
                    matching.where_clause.as_ref(),
                    &row,
                )?
            };
            if matched.is_empty() && matching.optional {
                // An OPTIONAL MATCH that matched nothing keeps the row and binds every
                // variable its patterns name to null.
                let mut unmatched = row;
                for name in pattern_variables(&matching.patterns) {
                    unmatched.entry(name).or_insert(CypherValue::Null);
                }
                out.push(unmatched);
            } else {
                out.extend(matched);
            }
            self.check_rows(out.len())?;
        }
        Ok(out)
    }

    /// `UNWIND <list> AS <variable>`: one row per item of the list.
    fn run_unwind(&mut self, unwind: &Unwind, rows: Vec<Row>) -> Result<Vec<Row>, QueryFailure> {
        let mut out = Vec::new();
        for row in rows {
            let value = {
                let reader = self.reader();
                reader.eval(&unwind.expr, &row, None)?
            };
            let items = match value {
                CypherValue::List(items) => items,
                // `UNWIND null` expands to no rows, which is what openCypher states.
                CypherValue::Null => Vec::new(),
                _ => {
                    return Err(QueryFailure::Refused(
                        "Type mismatch: UNWIND takes a list".to_string(),
                    ))
                }
            };
            for item in items {
                self.budget.charge(1)?;
                // The ceiling is taken BEFORE the row is built, so a list a request
                // carried cannot be expanded past it and then refused, and the copy is
                // charged for the bytes it takes rather than as one flat step.
                self.check_rows(out.len().saturating_add(1))?;
                self.budget.charge_copy(row_bytes(&row))?;
                let mut expanded = row.clone();
                expanded.insert(unwind.variable.0.clone(), item);
                out.push(expanded);
            }
            self.check_rows(out.len())?;
        }
        Ok(out)
    }

    // -------------------------------------------------------------- writing clauses

    /// `CREATE <patterns>`: the elements the patterns name, per incoming row.
    fn run_create(&mut self, create: &Create, rows: Vec<Row>) -> Result<Vec<Row>, QueryFailure> {
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let mut created = row;
            for part in &create.patterns {
                self.create_part(part, &mut created)?;
            }
            out.push(created);
        }
        Ok(out)
    }

    /// `MERGE <pattern>`: the rows the pattern already matches, or the elements it names
    /// created once, with the actions of the branch that was taken applied.
    fn run_merge(&mut self, merge: &Merge, rows: Vec<Row>) -> Result<Vec<Row>, QueryFailure> {
        let mut out = Vec::new();
        for row in rows {
            let matched = {
                let reader = self.reader();
                reader.match_patterns(::std::slice::from_ref(&merge.pattern), None, &row)?
            };
            let (branch, rows) = if matched.is_empty() {
                let mut created = row;
                self.create_part(&merge.pattern, &mut created)?;
                (::opencypher::MergeOn::Create, vec![created])
            } else {
                (::opencypher::MergeOn::Match, matched)
            };
            for action in &merge.actions {
                if action.on == branch {
                    self.apply_set(&action.set, &rows)?;
                }
            }
            out.extend(rows);
            self.check_rows(out.len())?;
        }
        Ok(out)
    }

    /// Create the elements one pattern part names, binding each to the variable the
    /// pattern gave it.
    fn create_part(&mut self, part: &PatternPart, row: &mut Row) -> Result<(), QueryFailure> {
        self.budget.charge(1)?;
        let element = &part.element;
        let mut path = Vec::new();
        let mut current = self.created_node(&element.start, row)?;
        path.push(CypherValue::Node(current.clone()));
        for (relationship, node) in &element.chain {
            let next = self.created_node(node, row)?;
            let created = self.created_relationship(relationship, &current, &next, row)?;
            path.push(CypherValue::Relationship(created));
            path.push(CypherValue::Node(next.clone()));
            current = next;
        }
        if let Some(variable) = &part.variable {
            row.insert(variable.0.clone(), CypherValue::List(path));
        }
        Ok(())
    }

    /// The node one node pattern of a creating pattern names: the one its variable is
    /// already bound to, or a node created from its labels and properties.
    fn created_node(
        &mut self,
        pattern: &::opencypher::NodePattern,
        row: &mut Row,
    ) -> Result<String, QueryFailure> {
        if let Some(variable) = &pattern.variable {
            if let Some(bound) = row.get(variable.0.as_str()) {
                let CypherValue::Node(id) = bound else {
                    return Err(QueryFailure::Refused(format!(
                        "Variable `{variable}` already bound to something that is not a node"
                    )));
                };
                if pattern.labels.is_some() || pattern.properties.is_some() {
                    return Err(QueryFailure::Refused(format!(
                        "Can't create node `{variable}` with labels or properties here: \
                         the variable is already bound"
                    )));
                }
                return Ok(id.clone());
            }
        }
        let labels = match &pattern.labels {
            Some(labels) => created_labels(labels)?,
            None => Vec::new(),
        };
        let mut properties = {
            let reader = self.reader();
            reader.pattern_properties(pattern.properties.as_ref(), row)?
        };
        let asked_id = requested_id(&mut properties)?;
        let id = self
            .graph
            .insert_node(asked_id, labels, properties)
            .map_err(QueryFailure::Write)?;
        if let Some(variable) = &pattern.variable {
            row.insert(variable.0.clone(), CypherValue::Node(id.clone()));
        }
        Ok(id)
    }

    /// The relationship one hop of a creating pattern names, created between the two
    /// nodes the pattern joined.
    fn created_relationship(
        &mut self,
        pattern: &::opencypher::RelationshipPattern,
        left: &str,
        right: &str,
        row: &mut Row,
    ) -> Result<String, QueryFailure> {
        let (from_node, to_node) = match pattern.direction {
            ::opencypher::Direction::Outgoing => (left, right),
            ::opencypher::Direction::Incoming => (right, left),
            // A relationship the store holds leaves one node and enters another, so a
            // pattern that does not say which way round cannot create one.
            _ => {
                return Err(QueryFailure::Refused(
                    "Only directed relationships are supported in CREATE".to_string(),
                ))
            }
        };
        if pattern.range.is_some() {
            return Err(QueryFailure::Refused(
                "A variable-length relationship is not supported in CREATE".to_string(),
            ));
        }
        let types = match &pattern.types {
            Some(types) => created_labels(types)?,
            None => Vec::new(),
        };
        let [rel_type] = types.as_slice() else {
            return Err(QueryFailure::Refused(
                "A relationship a query creates carries exactly one type".to_string(),
            ));
        };
        let mut properties = {
            let reader = self.reader();
            reader.pattern_properties(pattern.properties.as_ref(), row)?
        };
        let asked_id = requested_id(&mut properties)?;
        let id = self
            .graph
            .insert_relationship(asked_id, rel_type.clone(), from_node, to_node, properties)
            .map_err(QueryFailure::Write)?;
        if let Some(variable) = &pattern.variable {
            row.insert(variable.0.clone(), CypherValue::Relationship(id.clone()));
        }
        Ok(id)
    }

    /// `SET` (and a `MERGE` action): each assignment, for each row.
    fn apply_set(&mut self, items: &[SetItem], rows: &[Row]) -> Result<(), QueryFailure> {
        for row in rows {
            for item in items {
                self.budget.charge(1)?;
                match item {
                    SetItem::Property {
                        target,
                        value: assigned,
                        ..
                    } => {
                        let ::opencypher::ExprKind::Property { base, key } = &target.kind else {
                            return Err(QueryFailure::Refused(
                                "SET takes a property, a map or labels".to_string(),
                            ));
                        };
                        let (element, written) = {
                            let reader = self.reader();
                            let subject = reader.eval(base, row, None)?;
                            let value = reader.eval(assigned, row, None)?;
                            // Assigning null REMOVES the property: openCypher holds no
                            // property whose value is null.
                            let written = match value {
                                CypherValue::Null => None,
                                value => Some(property_json(&value)?),
                            };
                            (element_id(&subject)?, written)
                        };
                        let Some(element) = element else { continue };
                        written_key(key.0.as_str())?;
                        self.graph
                            .set_element_property(&element, key.0.as_str(), written)
                            .map_err(QueryFailure::Write)?;
                    }
                    SetItem::Replace {
                        variable,
                        value: assigned,
                        ..
                    }
                    | SetItem::Mutate {
                        variable,
                        value: assigned,
                        ..
                    } => {
                        let replacing = matches!(item, SetItem::Replace { .. });
                        let (element, written) = {
                            let reader = self.reader();
                            let subject =
                                row.get(variable.0.as_str()).cloned().ok_or_else(|| {
                                    QueryFailure::Refused(format!(
                                        "Variable `{variable}` not defined"
                                    ))
                                })?;
                            let value = reader.eval(assigned, row, None)?;
                            let CypherValue::Map(entries) = value else {
                                return Err(QueryFailure::Refused(
                                    "SET assigns a map of properties to a variable".to_string(),
                                ));
                            };
                            let mut written = BTreeMap::new();
                            for (key, entry) in entries {
                                written.insert(key, entry);
                            }
                            (element_id(&subject)?, written)
                        };
                        let Some(element) = element else { continue };
                        if replacing {
                            let mut properties = BTreeMap::new();
                            for (key, value) in &written {
                                written_key(key)?;
                                if matches!(value, CypherValue::Null) {
                                    continue;
                                }
                                properties.insert(key.clone(), property_json(value)?);
                            }
                            self.graph
                                .replace_element_properties(&element, properties)
                                .map_err(QueryFailure::Write)?;
                        } else {
                            for (key, value) in &written {
                                written_key(key)?;
                                let stored = match value {
                                    CypherValue::Null => None,
                                    value => Some(property_json(value)?),
                                };
                                self.graph
                                    .set_element_property(&element, key, stored)
                                    .map_err(QueryFailure::Write)?;
                            }
                        }
                    }
                    SetItem::Labels {
                        variable, labels, ..
                    } => {
                        let subject = row.get(variable.0.as_str()).cloned();
                        let Some(CypherValue::Node(id)) = subject else {
                            return Err(QueryFailure::Refused(format!(
                                "Type mismatch: `{variable}` must be a node to be labelled"
                            )));
                        };
                        for label in labels {
                            written_label(label.0.as_str())?;
                            self.graph
                                .add_node_label(&id, label.0.as_str())
                                .map_err(QueryFailure::Write)?;
                        }
                    }
                    _ => {
                        return Err(QueryFailure::Refused(
                            "Unsupported SET item in this query".to_string(),
                        ))
                    }
                }
            }
        }
        Ok(())
    }

    /// `REMOVE`: each label or property named, for each row.
    fn apply_remove(&mut self, items: &[RemoveItem], rows: &[Row]) -> Result<(), QueryFailure> {
        for row in rows {
            for item in items {
                self.budget.charge(1)?;
                match item {
                    RemoveItem::Labels {
                        variable, labels, ..
                    } => {
                        let Some(CypherValue::Node(id)) = row.get(variable.0.as_str()).cloned()
                        else {
                            return Err(QueryFailure::Refused(format!(
                                "Type mismatch: `{variable}` must be a node to have labels removed"
                            )));
                        };
                        for label in labels {
                            written_label(label.0.as_str())?;
                            self.graph.remove_node_label(&id, label.0.as_str());
                        }
                    }
                    RemoveItem::Property { target, .. } => {
                        let ::opencypher::ExprKind::Property { base, key } = &target.kind else {
                            return Err(QueryFailure::Refused(
                                "REMOVE takes a property or labels".to_string(),
                            ));
                        };
                        let element = {
                            let reader = self.reader();
                            let subject = reader.eval(base, row, None)?;
                            element_id(&subject)?
                        };
                        let Some(element) = element else { continue };
                        written_key(key.0.as_str())?;
                        self.graph
                            .set_element_property(&element, key.0.as_str(), None)
                            .map_err(QueryFailure::Write)?;
                    }
                    _ => {
                        return Err(QueryFailure::Refused(
                            "Unsupported REMOVE item in this query".to_string(),
                        ))
                    }
                }
            }
        }
        Ok(())
    }

    /// `DELETE` / `DETACH DELETE`: the elements the targets name.
    ///
    /// A node still joined to a relationship is NOT deleted unless the query said
    /// `DETACH`: the store would otherwise hold a relationship with an endpoint that is
    /// not there.
    fn apply_delete(&mut self, delete: &Delete, rows: &[Row]) -> Result<(), QueryFailure> {
        for row in rows {
            for target in &delete.targets {
                self.budget.charge(1)?;
                let value = {
                    let reader = self.reader();
                    reader.eval(target, row, None)?
                };
                match value {
                    CypherValue::Null => {}
                    CypherValue::Node(id) => {
                        let incident = self.graph.incident_relationship_ids(&id);
                        if !incident.is_empty() && !delete.detach {
                            return Err(QueryFailure::Constraint(
                                "Cannot delete node, because it still has relationships. \
                                 To delete this node, you must first delete its relationships."
                                    .to_string(),
                            ));
                        }
                        for relationship in incident {
                            self.graph.remove_relationship(&relationship);
                        }
                        self.graph.remove_node(&id);
                    }
                    CypherValue::Relationship(id) => {
                        self.graph.remove_relationship(&id);
                    }
                    _ => {
                        return Err(QueryFailure::Refused(
                            "Type mismatch: DELETE takes a node or a relationship".to_string(),
                        ))
                    }
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ projection

    /// Project `rows` through a `RETURN` or `WITH` body: its items, its aggregates, its
    /// ordering and its paging.
    fn project(
        &self,
        body: &ReturnBody,
        distinct: bool,
        rows: &[Row],
        where_clause: Option<&Expr>,
    ) -> Result<(Vec<String>, Vec<Row>), QueryFailure> {
        let mut projections: Vec<Projected<'_>> = Vec::new();
        if body.star {
            // `*` projects every variable in scope, in a stable order.
            let mut names: Vec<String> = Vec::new();
            for row in rows {
                for name in row.keys() {
                    if !names.iter().any(|held| held == name) {
                        names.push(name.clone());
                    }
                }
            }
            names.sort();
            projections.extend(names.into_iter().map(Projected::Bound));
        }
        for item in &body.items {
            projections.push(Projected::Computed {
                name: self.column_name(item),
                expr: &item.expr,
            });
        }
        if projections.is_empty() {
            return Err(QueryFailure::Refused(
                "A projection names at least one item".to_string(),
            ));
        }
        let columns: Vec<String> = projections
            .iter()
            .map(|projection| match projection {
                Projected::Bound(name) => name.clone(),
                Projected::Computed { name, .. } => name.clone(),
            })
            .collect();

        let aggregated = projections.iter().any(|projection| match projection {
            Projected::Bound(_) => false,
            Projected::Computed { expr, .. } => aggregates(expr),
        });
        // Each output row is kept beside the row it came from, so an `ORDER BY` can name
        // a variable the projection did not keep.
        let mut projected: Vec<(Row, Row)> = Vec::new();
        if aggregated {
            projected.extend(self.aggregated_rows(&projections, rows)?);
        } else {
            for row in rows {
                self.budget.charge(1)?;
                self.budget.charge_copy(row_bytes(row))?;
                let mut out = Row::new();
                for projection in &projections {
                    let (name, value) = self.projected_value(projection, row, None)?;
                    out.insert(name, value);
                }
                projected.push((out, row.clone()));
            }
        }

        if let Some(predicate) = where_clause {
            let mut kept = Vec::new();
            for (out, source) in projected {
                let scope = merged(&self.budget, &out, &source)?;
                if truth(&self.reader().eval(predicate, &scope, None)?) == Some(true) {
                    kept.push((out, source));
                }
            }
            projected = kept;
        }

        if !body.order_by.is_empty() {
            // The keys are computed once per row and then sorted, so a sort costs one
            // evaluation per row rather than one per comparison.
            let mut keyed: Vec<(Vec<CypherValue>, (Row, Row))> = Vec::new();
            for (out, source) in projected {
                let scope = merged(&self.budget, &out, &source)?;
                let mut keys = Vec::with_capacity(body.order_by.len());
                for sort in &body.order_by {
                    keys.push(self.reader().eval(&sort.expr, &scope, None)?);
                }
                keyed.push((keys, (out, source)));
            }
            keyed.sort_by(|left, right| {
                for (index, sort) in body.order_by.iter().enumerate() {
                    let (Some(one), Some(other)) = (left.0.get(index), right.0.get(index)) else {
                        continue;
                    };
                    let ordering = order(one, other);
                    let ordering = if sort.descending {
                        ordering.reverse()
                    } else {
                        ordering
                    };
                    if ordering != ::std::cmp::Ordering::Equal {
                        return ordering;
                    }
                }
                ::std::cmp::Ordering::Equal
            });
            projected = keyed.into_iter().map(|(_, pair)| pair).collect();
        }

        let mut out: Vec<Row> = projected.into_iter().map(|(out, _)| out).collect();
        if distinct {
            out = self.distinct(&columns, out);
        }
        if let Some(skip) = &body.skip {
            let skipped = self.paging_count(skip, "SKIP")?;
            out = out.into_iter().skip(skipped).collect();
        }
        if let Some(limit) = &body.limit {
            let taken = self.paging_count(limit, "LIMIT")?;
            out.truncate(taken);
        }
        self.check_rows(out.len())?;
        Ok((columns, out))
    }

    /// The rows an aggregating projection answers: one per group of the items that are
    /// not aggregates.
    fn aggregated_rows(
        &self,
        projections: &[Projected<'_>],
        rows: &[Row],
    ) -> Result<Vec<(Row, Row)>, QueryFailure> {
        let keys: Vec<&Projected<'_>> = projections
            .iter()
            .filter(|projection| match projection {
                Projected::Bound(_) => true,
                Projected::Computed { expr, .. } => !aggregates(expr),
            })
            .collect();
        // Groups in the order they were first seen, so the answer does not depend on how
        // a map happens to iterate.
        let mut order_seen: Vec<String> = Vec::new();
        let mut groups: BTreeMap<String, Vec<Row>> = BTreeMap::new();
        for row in rows {
            self.budget.charge(1)?;
            self.budget.charge_copy(row_bytes(row))?;
            let mut key_values = Vec::with_capacity(keys.len());
            for projection in &keys {
                let (_, value) = self.projected_value(projection, row, None)?;
                key_values.push(value);
            }
            let key = group_key(&key_values);
            if !groups.contains_key(&key) {
                order_seen.push(key.clone());
            }
            groups.entry(key).or_default().push(row.clone());
        }
        if groups.is_empty() {
            // An aggregate over no rows still answers one row — `count()` of nothing is
            // zero — but only when the projection groups by nothing.
            if !keys.is_empty() {
                return Ok(Vec::new());
            }
            let empty = Row::new();
            let mut out = Row::new();
            for projection in projections {
                let (name, value) = self.projected_value(projection, &empty, Some(&[]))?;
                out.insert(name, value);
            }
            return Ok(vec![(out, empty)]);
        }
        let mut out = Vec::with_capacity(order_seen.len());
        for key in order_seen {
            let Some(group) = groups.get(&key) else {
                continue;
            };
            let Some(representative) = group.first() else {
                continue;
            };
            let mut projected = Row::new();
            for projection in projections {
                let (name, value) = self.projected_value(projection, representative, Some(group))?;
                projected.insert(name, value);
            }
            self.budget.charge_copy(row_bytes(representative))?;
            out.push((projected, representative.clone()));
        }
        Ok(out)
    }

    /// One projected item's name and value.
    fn projected_value(
        &self,
        projection: &Projected<'_>,
        row: &Row,
        group: Option<&[Row]>,
    ) -> Result<(String, CypherValue), QueryFailure> {
        match projection {
            Projected::Bound(name) => Ok((
                name.clone(),
                row.get(name.as_str()).cloned().unwrap_or(CypherValue::Null),
            )),
            Projected::Computed { name, expr } => {
                Ok((name.clone(), self.reader().eval(expr, row, group)?))
            }
        }
    }

    /// The name one projected item answers under: its `AS` alias, or the text the query
    /// wrote the expression with, which is what openCypher names an unaliased column by.
    fn column_name(&self, item: &::opencypher::ReturnItem) -> String {
        match &item.alias {
            Some(alias) => alias.0.clone(),
            None => {
                let written = item.expr.span.slice(self.source).trim();
                if written.is_empty() {
                    "expression".to_string()
                } else {
                    written.to_string()
                }
            }
        }
    }

    /// `SKIP` / `LIMIT`: a count the query wrote, which openCypher requires to be a
    /// non-negative integer this engine can page by.
    fn paging_count(&self, expr: &Expr, clause: &str) -> Result<usize, QueryFailure> {
        let value = self.reader().eval(expr, &Row::new(), None)?;
        match value {
            CypherValue::Int(count) if count >= 0 => {
                usize::try_from(count).map_err(|_| {
                    QueryFailure::Refused(format!(
                        "{clause} is larger than the number of rows this engine pages"
                    ))
                })
            }
            _ => Err(QueryFailure::Refused(format!(
                "{clause} takes a non-negative integer"
            ))),
        }
    }

    /// `rows` with the duplicates of their projected columns dropped, keeping the first
    /// of each.
    fn distinct(&self, columns: &[String], rows: Vec<Row>) -> Vec<Row> {
        let mut seen: ::std::collections::BTreeSet<String> = ::std::collections::BTreeSet::new();
        let mut kept = Vec::with_capacity(rows.len());
        for row in rows {
            let values: Vec<CypherValue> = columns
                .iter()
                .map(|column| row.get(column.as_str()).cloned().unwrap_or(CypherValue::Null))
                .collect();
            if seen.insert(group_key(&values)) {
                kept.push(row);
            }
        }
        kept
    }

    /// Refuse a result larger than this engine answers with, BEFORE it is rendered.
    fn check_rows(&self, rows: usize) -> Result<(), QueryFailure> {
        if rows > MAX_QUERY_RESULT_ROWS {
            return Err(QueryFailure::TooLarge {
                rows,
                ceiling: MAX_QUERY_RESULT_ROWS,
            });
        }
        Ok(())
    }

    /// One result set as the `results` member carries it: an object per row, keyed by the
    /// query's own column names, in the order the query projected them.
    ///
    /// The whole answer is bounded as it is built ([`MAX_RESULT_CHARS`]): the rows are
    /// capped and each value in them is capped, and this is the bound on their product, so
    /// a small request cannot be answered with an arbitrarily large response.
    fn rendered(&self, result: &ResultSet) -> Result<Vec<Value>, QueryFailure> {
        let reader = self.reader();
        let mut rendered = Vec::with_capacity(result.rows.len());
        let mut characters: usize = 0;
        for row in &result.rows {
            self.budget.charge(1)?;
            let mut object = ::serde_json::Map::new();
            for column in &result.columns {
                let value = row.get(column.as_str()).cloned().unwrap_or(CypherValue::Null);
                let rendered = reader.document(&value)?;
                characters = characters
                    .saturating_add(column.len())
                    .saturating_add(rendered.to_string().len());
                if characters > MAX_RESULT_CHARS {
                    return Err(QueryFailure::ValueTooLarge {
                        size: characters,
                        ceiling: MAX_RESULT_CHARS,
                    });
                }
                object.insert(column.clone(), rendered);
            }
            rendered.push(Value::Object(object));
        }
        Ok(rendered)
    }
}

/// A projected row read together with the row it came from: what the projection bound
/// wins, and the source row answers for a variable the projection did not keep.
///
/// The copy is charged for the bytes it takes, like every other copy of a row.
fn merged(budget: &Budget, projected: &Row, source: &Row) -> Result<Row, QueryFailure> {
    budget.charge_copy(row_bytes(source).saturating_add(row_bytes(projected)))?;
    let mut scope = source.clone();
    for (name, value) in projected {
        scope.insert(name.clone(), value.clone());
    }
    Ok(scope)
}

/// Whether `expr` contains an aggregating function anywhere inside it, which is what
/// makes a projection group its other items.
fn aggregates(expr: &Expr) -> bool {
    /// Looks for an aggregate anywhere in an expression tree.
    struct Aggregated(bool);
    impl Visitor for Aggregated {
        fn visit_expr(&mut self, expr: &Expr) {
            match &expr.kind {
                ::opencypher::ExprKind::CountStar => self.0 = true,
                ::opencypher::ExprKind::FunctionCall { name, .. }
                    if is_aggregate(name.0.as_str()) =>
                {
                    self.0 = true;
                }
                _ => {}
            }
            ::opencypher::walk_expr(self, expr);
        }
    }
    let mut found = Aggregated(false);
    found.visit_expr(expr);
    found.0
}

/// Every variable the patterns of one clause name — nodes, relationships and paths —
/// which is what an `OPTIONAL MATCH` that matched nothing binds to null.
fn pattern_variables(patterns: &[PatternPart]) -> Vec<String> {
    let mut names = Vec::new();
    let mut remember = |variable: Option<&::opencypher::Variable>| {
        if let Some(variable) = variable {
            names.push(variable.0.clone());
        }
    };
    for part in patterns {
        remember(part.variable.as_ref());
        remember(part.element.start.variable.as_ref());
        for (relationship, node) in &part.element.chain {
            remember(relationship.variable.as_ref());
            remember(node.variable.as_ref());
        }
    }
    names
}

/// The id of the element `value` names, `None` for `null` — which a `SET` on an
/// unmatched optional row carries, and which assigns nothing.
fn element_id(value: &CypherValue) -> Result<Option<String>, QueryFailure> {
    match value {
        CypherValue::Null => Ok(None),
        CypherValue::Node(id) | CypherValue::Relationship(id) => Ok(Some(id.clone())),
        _ => Err(QueryFailure::Refused(
            "Type mismatch: expected a node or a relationship".to_string(),
        )),
    }
}

/// The id a creating pattern asked the new element to be addressed by, taken OUT of the
/// properties it wrote: `~id` names the element rather than being a property of it.
fn requested_id(properties: &mut BTreeMap<String, Value>) -> Result<Option<String>, QueryFailure> {
    let Some(asked) = properties.remove(super::property_graph::ELEMENT_ID_MEMBER) else {
        return Ok(None);
    };
    match asked {
        Value::String(id) if !id.is_empty() => Ok(Some(id)),
        _ => Err(QueryFailure::Constraint(format!(
            "`{}` names the element being created and must be a non-empty string",
            super::property_graph::ELEMENT_ID_MEMBER
        ))),
    }
}

/// Refuse a property name an element reports its OWN members with.
///
/// The members of an element — its id, the kind of element it is, its labels, its type,
/// its endpoints — are spelled with a leading `~`, which is what keeps them apart from the
/// element's property names. A creating pattern names the element with `~id`
/// ([`requested_id`]) and every other reserved member is the store's to report, so a
/// mutating clause may write none of them: an element reporting an id it is not keyed by
/// would mislead every later read of it.
fn written_key(key: &str) -> Result<(), QueryFailure> {
    if key.starts_with('~') {
        return Err(QueryFailure::Constraint(format!(
            "`{key}` is a member this engine reports for an element and not a property \
             a query may write"
        )));
    }
    Ok(())
}

/// Refuse a label a query cannot write: an element carries labels that NAME something, so
/// an empty one is not a label.
fn written_label(label: &str) -> Result<(), QueryFailure> {
    if label.is_empty() {
        return Err(QueryFailure::Constraint(
            "A label is a name, so it cannot be empty".to_string(),
        ));
    }
    Ok(())
}

/// The result rows as the operation's `results` member carries them.
#[must_use]
pub fn results_document(rows: &[Value]) -> ::aws_smithy_types::Document {
    ::aws_smithy_types::Document::Array(rows.iter().map(json_to_document).collect())
}

/// One JSON value as the `Document` the model declares for `results`.
fn json_to_document(value: &Value) -> ::aws_smithy_types::Document {
    use ::aws_smithy_types::{Document, Number};
    match value {
        Value::Null => Document::Null,
        Value::Bool(flag) => Document::Bool(*flag),
        Value::Number(number) => Document::Number(
            number
                .as_u64()
                .map(Number::PosInt)
                .or_else(|| number.as_i64().map(Number::NegInt))
                .or_else(|| number.as_f64().map(Number::Float))
                .unwrap_or(Number::PosInt(0)),
        ),
        Value::String(text) => Document::String(text.clone()),
        Value::Array(items) => Document::Array(items.iter().map(json_to_document).collect()),
        Value::Object(members) => Document::Object(
            members
                .iter()
                .map(|(key, member)| (key.clone(), json_to_document(member)))
                .collect(),
        ),
    }
}

/// The `MalformedQueryException` payload for a text this engine could not read as an
/// openCypher query, or could read and not evaluate.
///
/// `reason` is the whole message: what the parser or the evaluation reported, worded the
/// way the openCypher endpoint words it (see [`super::open_cypher_parse_failure`]), so the
/// caller can fix the query from the message alone. `code` carries the HTTP status the
/// member's own documentation says it carries.
pub fn malformed_open_cypher_query(reason: &str) -> MalformedQueryException {
    MalformedQueryException {
        detailed_message: reason.to_string(),
        request_id: super::new_request_id(),
    }
}

/// The `QueryTooLargeException` payload for a query text longer than this engine reads.
pub fn open_cypher_query_too_large(reason: &str) -> QueryTooLargeException {
    QueryTooLargeException {
        detailed_message: format!("openCypher query is too large: {reason}"),
        request_id: super::new_request_id(),
    }
}

/// The `QueryLimitException` payload for a query past a system limit on the work it may
/// do or the answer it may carry.
pub fn open_cypher_query_limit(reason: &str) -> QueryLimitException {
    QueryLimitException {
        detailed_message: format!("openCypher query exceeds a system limit: {reason}"),
        request_id: super::new_request_id(),
    }
}

/// The `TimeLimitExceededException` payload for a run whose span ran out.
pub fn open_cypher_time_limit(reason: &str) -> TimeLimitExceededException {
    TimeLimitExceededException {
        detailed_message: format!("openCypher query exceeded the time limit: {reason}"),
        request_id: super::new_request_id(),
    }
}

/// The `ConstraintViolationException` payload for a write the graph's own rules do not
/// allow.
pub fn open_cypher_constraint(reason: &str) -> ConstraintViolationException {
    ConstraintViolationException {
        detailed_message: reason.to_string(),
        request_id: super::new_request_id(),
    }
}

/// The `MemoryLimitExceededException` payload for a write the store had no room for.
pub fn open_cypher_memory_limit(reason: &str) -> MemoryLimitExceededException {
    MemoryLimitExceededException {
        detailed_message: format!("openCypher query ran out of room: {reason}"),
        request_id: super::new_request_id(),
    }
}

/// The `InvalidParameterException` payload for a `parameters` member this engine could
/// not read, or a parameter the query referenced and the request did not carry.
pub fn invalid_open_cypher_parameters(reason: &str) -> InvalidParameterException {
    InvalidParameterException {
        detailed_message: reason.to_string(),
        request_id: super::new_request_id(),
    }
}

/// The `UnsupportedOperationException` payload for a clause this engine does not run.
pub fn open_cypher_unsupported(clause: &str) -> UnsupportedOperationException {
    UnsupportedOperationException {
        detailed_message: format!("openCypher {clause} is not supported by this engine"),
        request_id: super::new_request_id(),
    }
}

/// The `FailureByQueryException` payload for a run this engine could not carry out at
/// all — a server-side failure, not anything the caller sent.
pub fn engine_failure(reason: &str) -> FailureByQueryException {
    FailureByQueryException {
        detailed_message: reason.to_string(),
        request_id: super::new_request_id(),
    }
}
