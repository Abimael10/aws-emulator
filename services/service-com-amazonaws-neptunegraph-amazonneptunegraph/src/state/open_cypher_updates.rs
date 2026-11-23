// SPDX-License-Identifier: CC0-1.0

//! The UPDATING half of the openCypher data plane: a query that WRITES the graph, and
//! one that binds its data through `$name` parameters.
//!
//! `ExecuteQuery` runs "READ AND WRITE openCypher" on a Neptune Analytics graph — the
//! operation's own documentation names the three IAM actions it is authorized against
//! (`neptune-graph:ReadDataViaQuery`, `neptune-graph:WriteDataViaQuery`,
//! `neptune-graph:DeleteDataViaQuery`), and `planCache` states the plan cache "works for
//! both READ and UPDATE queries"
//! (`smithy_query(query="doc",shape="ExecuteQuery")`,
//! `smithy_query(query="doc",shape="ExecuteQueryInput",member="planCache")`). So a
//! well-formed `CREATE`, `MERGE`, `SET`, `REMOVE` or `DELETE` is a query this endpoint
//! RUNS, and the data it writes is the data every later reading of the graph — a `MATCH`,
//! a summary, a snapshot, a reset — answers from.
//!
//! Two decisions live here, and nothing else does:
//!
//! * WHICH engine reads a query ([`beyond_the_reading_engine`]). The engine
//!   [`super::run_query_until`] evaluates through covers the read-only subset: it has no
//!   production for an updating clause or for a `$name` reference, and no value for a
//!   relationship a pattern traverses. So a text carrying any of the three is read by the
//!   openCypher parser that does ([`::opencypher`]) and evaluated by the engine that does
//!   — the one Neptune's other openCypher query endpoint owns, over the property graph in
//!   [`graph_engine`]. Both Neptune query endpoints run the SAME language, so there is one
//!   engine for it rather than a second copy here.
//! * HOW this graph's stored data is handed to that engine and taken back from it
//!   ([`as_property_graph`], [`commit`]). The conversion is by id: a node or an edge keeps
//!   the id it is addressed by, so a query reports the same `~id` a summary, an export and
//!   a later query report, and an element the query created keeps the id the engine minted
//!   for it.
//!
//! ## What a write costs, and when it lands
//!
//! A query that writes is evaluated with the store's guard HELD, so two callers writing at
//! once are applied one whole query at a time: that is what makes a `MERGE` on an upsert
//! path answer one node rather than one per caller. What is done under that guard is
//! bounded BEFORE it starts and while it runs — the text's length and nesting
//! ([`super::MAX_QUERY_STRING_CHARS`], [`super::MAX_QUERY_NESTING_DEPTH`]) are taken ahead
//! of either parse, and the evaluation spends the engine's own work budget and stops at
//! the deadline the request was given ([`graph_engine::MAX_QUERY_WORK_UNITS`],
//! [`super::query_run_timeout`]) — so one caller's query cannot hold the store for longer
//! than that however much it asks for. The evaluation itself reaches nothing but the data
//! it was handed: it opens no socket, reads no file and starts no process, and the clauses
//! that would reach outside a graph are refused rather than run
//! ([`super::UNSUPPORTED_CLAUSES`], and a function neither engine implements is refused by
//! the engine itself).
//!
//! The engine evaluates against a copy and the whole query either finishes or changes
//! nothing; what it left behind is then written through the graph's OWN writers
//! ([`super::GraphData::insert_node`], [`super::GraphData::insert_edge`]), so a write past
//! this graph's ceilings is refused with the ceiling it met and the stored data is left
//! exactly as it was.
//!
//! What a property may HOLD is this graph's own rule and is applied there too
//! ([`scalar_properties`]): a property value is one of openCypher's scalars, so a write
//! that sets one to a list or a map is refused rather than stored.

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime};

use ::serde_json::Value;
use ::service_com_amazonaws_neptunedata_amazonneptunedataplane::state as graph_engine;

use super::{GraphData, GraphEdge, GraphNode, QueryCancellation, QueryFailure, Stopped};

/// What the request's `parameters` member binds, by name.
///
/// "The data parameters the query can use in JSON format"
/// (`smithy_query(query="doc",shape="ExecuteQueryInput",member="parameters")`), so each
/// entry is the JSON document the request carried and the query's `$name` references bind
/// to it as DATA — no value is ever spliced into the query text.
pub type QueryParameters = BTreeMap<String, Value>;

/// The deepest a parameter value is read through.
///
/// Emulator resource guard: `parameters` targets a map of `Document`, which nests
/// arbitrarily, and reading one descends through it. A value nesting deeper is refused
/// rather than read, so no request can recurse this process through a parameter.
pub const MAX_PARAMETER_DEPTH: usize = 32;

/// The most characters the parameters of one request may carry, which is the ceiling the
/// engine that binds them states for exactly this ([`graph_engine::MAX_PARAMETERS_CHARS`]):
/// every value a `$name` binds comes out of them, so bounding them bounds what one request
/// can hand an evaluation.
pub const MAX_PARAMETERS_CHARS: usize = graph_engine::MAX_PARAMETERS_CHARS;

/// The parameters the request carried, as the data a `$name` binds to.
///
/// `Err` carries the sentence the operation refuses the request with: a request whose
/// `parameters` carry more than one evaluation may be handed, or a value this service
/// cannot read, is told so rather than having the member read as no parameters at all —
/// a query referencing `$name` would then be refused for a parameter the caller did
/// send.
pub fn request_parameters(
    supplied: Option<&::std::collections::HashMap<String, ::aws_smithy_types::Document>>,
) -> Result<QueryParameters, String> {
    let Some(supplied) = supplied else {
        return Ok(QueryParameters::new());
    };
    let mut parameters = QueryParameters::new();
    for (name, document) in supplied {
        let Some(value) = parameter_value(document, MAX_PARAMETER_DEPTH) else {
            return Err(format!(
                "the parameter ${name} nests deeper than the {MAX_PARAMETER_DEPTH} levels \
                 this endpoint reads a parameter through"
            ));
        };
        parameters.insert(name.clone(), value);
    }
    let characters = Value::Object(
        parameters
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    )
    .to_string()
    .chars()
    .count();
    if characters > MAX_PARAMETERS_CHARS {
        return Err(format!(
            "parameters carry {characters} characters and this endpoint binds at most \
             {MAX_PARAMETERS_CHARS}"
        ));
    }
    Ok(parameters)
}

/// One parameter value as the data it is, or `None` for one nesting past `depth`.
///
/// A `Document` and a JSON value hold the same four scalars, the same list and the same
/// object, so nothing about a value changes on the way in: the number form it was sent
/// with is the number form the query binds and the graph stores.
fn parameter_value(document: &::aws_smithy_types::Document, depth: usize) -> Option<Value> {
    use ::aws_smithy_types::{Document, Number};
    if depth == 0 {
        return None;
    }
    let value = match document {
        Document::Null => Value::Null,
        Document::Bool(flag) => Value::Bool(*flag),
        Document::String(text) => Value::String(text.clone()),
        Document::Number(Number::PosInt(number)) => Value::from(*number),
        Document::Number(Number::NegInt(number)) => Value::from(*number),
        Document::Number(Number::Float(number)) => ::serde_json::Number::from_f64(*number)
            .map_or(Value::Null, Value::Number),
        Document::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| parameter_value(item, depth - 1))
                .collect::<Option<Vec<Value>>>()?,
        ),
        Document::Object(members) => Value::Object(
            members
                .iter()
                .map(|(key, member)| {
                    parameter_value(member, depth - 1).map(|value| (key.clone(), value))
                })
                .collect::<Option<::serde_json::Map<String, Value>>>()?,
        ),
    };
    Some(value)
}

/// Run one request's query and answer the payload it is served with.
///
/// The text's own bounds are taken FIRST, for every query whichever engine reads it — how
/// long it is, how deep it nests, how many tokens it carries and how many rows its own
/// literals could make: the first three bound how far a parse of a caller-chosen text
/// descends, so neither parser, and not the parse that chooses between them, ever sees a
/// text past them.
///
/// Then the text decides which engine evaluates it:
///
/// * a query carrying an updating clause, a `$name` reference or a relationship its
///   pattern traverses is evaluated against the graph's STORED data, with the store's
///   guard held, and what it wrote is committed before the caller is answered;
/// * every other query is evaluated over `snapshot` — the data read when the request was
///   admitted — off the store's lock, exactly as it was before.
pub fn evaluate_query(
    state: &::engine::ServiceState<super::ComAmazonawsNeptunegraphAmazonneptunegraphState>,
    graph_id: &str,
    snapshot: &GraphData,
    query: &str,
    parameters: QueryParameters,
    deadline: Instant,
    cancellation: &QueryCancellation,
) -> Result<Vec<u8>, QueryFailure> {
    let characters = query.chars().count();
    if characters > super::MAX_QUERY_STRING_CHARS {
        return Err(QueryFailure::TooLong {
            characters,
            ceiling: super::MAX_QUERY_STRING_CHARS,
        });
    }
    let depth = super::nesting_depth(query);
    if depth > super::MAX_QUERY_NESTING_DEPTH {
        return Err(QueryFailure::TooDeep {
            depth,
            ceiling: super::MAX_QUERY_NESTING_DEPTH,
        });
    }
    // A text's BRACKETS are not the only thing a recursive-descent parse descends
    // through, so the depth above is paired with the count of tokens the text carries at
    // all: a chain of prefix or infix constructs nests without a bracket in it, and the
    // stack one run is given is sized for this count. It is taken HERE, before the parse
    // that decides which engine reads the text, so no parse of this service's ever
    // descends through more than the two bounds together admit.
    let tokens = super::token_count(query);
    if tokens > super::MAX_QUERY_TOKENS {
        return Err(QueryFailure::TooManyTokens {
            tokens,
            ceiling: super::MAX_QUERY_TOKENS,
        });
    }
    // The rows the text's own literals could expand to are counted from the text and
    // refused before an engine builds any of them, for the same reason: they are rows no
    // reading of the graph is charged for.
    let literal_rows = super::literal_rows(query);
    if literal_rows > super::MAX_QUERY_RESULT_ROWS {
        return Err(QueryFailure::TooLarge {
            rows: literal_rows,
            ceiling: super::MAX_QUERY_RESULT_ROWS,
        });
    }
    if !beyond_the_reading_engine(query) {
        return super::run_query_until(snapshot, query, deadline, cancellation);
    }
    // The guard is taken for the whole evaluation: a query that writes is applied one
    // whole query at a time, which is what makes two callers merging the same element
    // answer one element. The clock is read UNDER the guard, so the statistics over the
    // data are recomputed as of the instant this write holds it at.
    let (mut store, _taken_at) = super::locked_store_now(state);
    let as_of = SystemTime::now();
    let Some(graph) = store.graphs.get_mut(graph_id) else {
        return Err(QueryFailure::Interrupted(
            "the graph this query was running on was deleted while it ran".to_string(),
        ));
    };
    run_updating_query(&mut graph.data, query, parameters, deadline, as_of)
}

/// Whether the query text carries something the reading engine cannot evaluate
/// faithfully: an updating clause, a `$name` parameter reference, or a RELATIONSHIP its
/// pattern traverses.
///
/// The three are one rule — a query is evaluated by the engine whose grammar and value
/// domain cover what it says and what it binds. The reading engine has no production for
/// an updating clause or a parameter, and no VALUE for a relationship: it answers about
/// one only by the nodes at its ends, so the type a pattern names, the properties set on
/// a relationship and whether the graph holds one at all are questions for the engine
/// whose value domain holds relationships.
///
/// Decided from the AST a real openCypher parser produces, never from the text: a
/// keyword inside a string literal or a comment is not a clause, and a `$` inside one is
/// not a parameter. A text that does not parse at all is NOT claimed here — the reading
/// engine words a refusal with the position it stopped at and the constructs its grammar
/// admits there ([`super::syntax_refusal`]), so an unreadable text stays with it.
#[must_use]
pub fn beyond_the_reading_engine(query: &str) -> bool {
    let Ok(parsed) = ::opencypher::parse(query) else {
        return false;
    };
    let mut surface = Surface::default();
    ::opencypher::Visitor::visit_query(&mut surface, &parsed);
    surface.updates || surface.parameters || surface.relationships
}

/// What a query SAYS, as far as choosing an engine for it needs to know: whether it
/// updates the graph, binds a parameter, or binds a relationship. Collected over the
/// whole AST, nested clauses and expressions included.
#[derive(Debug, Default)]
struct Surface {
    /// Whether any clause of the query changes the graph.
    updates: bool,
    /// Whether any part of the query references a `$name` parameter.
    parameters: bool,
    /// Whether any pattern of the query traverses a relationship.
    relationships: bool,
}

impl ::opencypher::Visitor for Surface {
    fn visit_clause(&mut self, clause: &::opencypher::Clause) {
        if matches!(
            clause,
            ::opencypher::Clause::Create(_)
                | ::opencypher::Clause::Merge(_)
                | ::opencypher::Clause::Set(_)
                | ::opencypher::Clause::Remove(_)
                | ::opencypher::Clause::Delete(_)
                | ::opencypher::Clause::Foreach(_)
        ) {
            self.updates = true;
        }
        ::opencypher::walk_clause(self, clause);
    }

    fn visit_expr(&mut self, expr: &::opencypher::Expr) {
        if matches!(expr.kind, ::opencypher::ExprKind::Parameter(_)) {
            self.parameters = true;
        }
        ::opencypher::walk_expr(self, expr);
    }

    fn visit_node_pattern(&mut self, node: &::opencypher::NodePattern) {
        if matches!(node.properties, Some(::opencypher::Properties::Param(_))) {
            self.parameters = true;
        }
        ::opencypher::walk_node_pattern(self, node);
    }

    fn visit_relationship_pattern(&mut self, relationship: &::opencypher::RelationshipPattern) {
        if matches!(
            relationship.properties,
            Some(::opencypher::Properties::Param(_))
        ) {
            self.parameters = true;
        }
        // A pattern that traverses a relationship AT ALL: the reading engine answers
        // about a relationship only by the nodes at its ends, so the type a pattern
        // names, the properties on it, and whether the graph holds one of that type, are
        // all questions for the engine whose value domain holds relationships.
        self.relationships = true;
        ::opencypher::walk_relationship_pattern(self, relationship);
    }
}

/// Run `query` against `data` and commit whatever it wrote.
///
/// The evaluation itself is the engine's, over the graph's own nodes and edges; what this
/// adds is the conversion in and out and the graph's own ceilings on the data it may be
/// left holding. A refusal — the engine's or a ceiling's — leaves `data` exactly as it
/// was, so a query that could not be finished writes nothing.
fn run_updating_query(
    data: &mut GraphData,
    query: &str,
    parameters: QueryParameters,
    deadline: Instant,
    as_of: SystemTime,
) -> Result<Vec<u8>, QueryFailure> {
    let mut graph = as_property_graph(data)?;
    let rows = graph_engine::run_query(&mut graph, query, parameters, deadline)
        .map_err(engine_refusal)?;
    commit(data, &graph, as_of)?;
    Ok(super::rows_payload(&rows))
}

/// This graph's stored data as the engine's property graph: the same nodes and edges,
/// each under the id it is addressed by, in the store's own order.
fn as_property_graph(data: &GraphData) -> Result<graph_engine::PropertyGraph, QueryFailure> {
    let mut graph = graph_engine::PropertyGraph::default();
    for (id, node) in &data.nodes {
        graph
            .insert_node(
                Some(id.clone()),
                node.labels.iter().cloned().collect(),
                node.properties.clone(),
            )
            .map_err(write_refusal)?;
    }
    for (id, edge) in &data.edges {
        graph
            .insert_relationship(
                Some(id.clone()),
                edge.label.clone(),
                &edge.from_node,
                &edge.to_node,
                edge.properties.clone(),
            )
            .map_err(write_refusal)?;
    }
    Ok(graph)
}

/// Write what the evaluation left behind back into the graph's data, through the graph's
/// OWN writers — so an openCypher write and a bulk load store the same kind of record and
/// meet the same ceilings, and the statistics over the data are recomputed as of `as_of`.
///
/// Built beside the stored data and assigned only once every element has been taken, so a
/// graph that cannot hold what the query wrote keeps the data it had.
///
/// An evaluation that left the elements EXACTLY as they were writes nothing at all: a
/// query that only read the graph, a `MERGE` that matched, a `DELETE` that matched no
/// element. The statistics over a graph's data are a function of that data and are
/// recomputed when it changes and not otherwise ([`super::GraphData`]), and the instant
/// they were computed at is reported on the wire, so a query that changed nothing must
/// leave it where it was.
fn commit(
    data: &mut GraphData,
    graph: &graph_engine::PropertyGraph,
    as_of: SystemTime,
) -> Result<(), QueryFailure> {
    let mut written = GraphData::empty(as_of);
    for id in graph.node_ids() {
        let Some(node) = graph.node(&id) else {
            continue;
        };
        written
            .insert_node(
                &id,
                GraphNode {
                    labels: node.labels.iter().cloned().collect(),
                    properties: node.properties.clone(),
                },
                as_of,
            )
            .map_err(QueryFailure::OverLimit)?;
    }
    for id in graph.relationship_ids() {
        let Some(edge) = graph.relationship(&id) else {
            continue;
        };
        written
            .insert_edge(
                &id,
                GraphEdge {
                    label: edge.rel_type.clone(),
                    from_node: edge.from_node.clone(),
                    to_node: edge.to_node.clone(),
                    properties: edge.properties.clone(),
                },
                as_of,
            )
            .map_err(QueryFailure::OverLimit)?;
    }
    scalar_properties(&written, data)?;
    if written.nodes == data.nodes && written.edges == data.edges {
        return Ok(());
    }
    *data = written;
    Ok(())
}

/// Refuse a write that set a property to something that is not a property VALUE.
///
/// A property of a graph element holds one of openCypher's scalars — null, a boolean, a
/// number or a string. A list and a map are values a query may compute, bind and answer
/// with, and neither is a property value, so a write that stores one is refused rather
/// than taken: a graph holding one would answer it back to every later read of that
/// element.
///
/// What a write is judged on is what it WROTE: a value `before` already held under the
/// same element and key is left alone, so data another path stored (a bulk load, a
/// restore) is not re-judged by a query that touched something else.
fn scalar_properties(written: &GraphData, before: &GraphData) -> Result<(), QueryFailure> {
    for (id, node) in &written.nodes {
        let stored = before.nodes.get(id);
        for (key, value) in &node.properties {
            if is_scalar(value)
                || stored.and_then(|stored| stored.properties.get(key)) == Some(value)
            {
                continue;
            }
            return Err(QueryFailure::Constraint(
                NON_SCALAR_PROPERTY_MESSAGE.to_string(),
            ));
        }
    }
    for (id, edge) in &written.edges {
        let stored = before.edges.get(id);
        for (key, value) in &edge.properties {
            if is_scalar(value)
                || stored.and_then(|stored| stored.properties.get(key)) == Some(value)
            {
                continue;
            }
            return Err(QueryFailure::Constraint(
                NON_SCALAR_PROPERTY_MESSAGE.to_string(),
            ));
        }
    }
    Ok(())
}

/// Whether `value` is a value a property holds: one of openCypher's scalars.
fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

/// What a write that set a property to something other than a simple literal is refused
/// with.
///
/// ONE constant, interpolating nothing from the request: what the service states is that
/// a property value has to be a simple literal, and it names the COLLECTION type it found
/// there instead — the one type both the list and the map form of a value are — rather
/// than the property or the value the query wrote.
const NON_SCALAR_PROPERTY_MESSAGE: &str = "Expected a simple literal but found CType.";

/// How the engine's own refusal of a query is reported through this service's failures.
///
/// Each kind stays the fact it is: a text that could not be READ, a clause this endpoint
/// does not run, a query that parses and cannot be EVALUATED, a parameter the request did
/// not carry, a bound the run met, and a run that could not be carried out at all.
fn engine_refusal(failure: graph_engine::QueryFailure) -> QueryFailure {
    match failure {
        graph_engine::QueryFailure::Malformed { reason } => QueryFailure::Malformed(reason),
        graph_engine::QueryFailure::Unsupported { clause } => QueryFailure::Unsupported { clause },
        graph_engine::QueryFailure::Refused(reason) => QueryFailure::Rejected(reason),
        graph_engine::QueryFailure::Constraint(reason) => QueryFailure::Constraint(reason),
        graph_engine::QueryFailure::MissingParameter(name) => QueryFailure::MissingParameter(name),
        graph_engine::QueryFailure::TooLong {
            characters,
            ceiling,
        } => QueryFailure::TooLong {
            characters,
            ceiling,
        },
        graph_engine::QueryFailure::TooDeep { depth, ceiling } => {
            QueryFailure::TooDeep { depth, ceiling }
        }
        graph_engine::QueryFailure::TooLarge { rows, ceiling } => {
            QueryFailure::TooLarge { rows, ceiling }
        }
        graph_engine::QueryFailure::ValueTooLarge { size, ceiling } => QueryFailure::OverLimit(
            format!("the query builds a value of {size} and this endpoint builds at most {ceiling}"),
        ),
        graph_engine::QueryFailure::TooNested { ceiling } => QueryFailure::OverLimit(format!(
            "the query evaluates deeper than the {ceiling} levels one query may descend"
        )),
        graph_engine::QueryFailure::TooManyHops { asked, ceiling } => {
            QueryFailure::OverLimit(format!(
                "the query walks up to {asked} hops and this endpoint walks at most {ceiling}"
            ))
        }
        graph_engine::QueryFailure::Exhausted { .. } => QueryFailure::Stopped(Stopped::Exhausted),
        graph_engine::QueryFailure::TimedOut { .. } => QueryFailure::Stopped(Stopped::TimedOut),
        graph_engine::QueryFailure::Write(refused) => write_refusal(refused),
        graph_engine::QueryFailure::Interrupted(reason) => QueryFailure::Interrupted(reason),
    }
}

/// How a write the property graph could not take is reported: a store with no room left
/// is a limit this emulator met, and a write the graph's own rules refuse is a constraint
/// the query did not satisfy.
fn write_refusal(refused: graph_engine::GraphWriteError) -> QueryFailure {
    match refused {
        graph_engine::GraphWriteError::NodeLimit { ceiling } => QueryFailure::OverLimit(format!(
            "this emulator holds at most {ceiling} nodes per graph"
        )),
        graph_engine::GraphWriteError::RelationshipLimit { ceiling } => QueryFailure::OverLimit(
            format!("this emulator holds at most {ceiling} edges per graph"),
        ),
        graph_engine::GraphWriteError::ByteLimit { ceiling } => QueryFailure::OverLimit(format!(
            "this emulator holds at most {ceiling} bytes of element data per graph"
        )),
        graph_engine::GraphWriteError::LabelLimit { ceiling } => {
            QueryFailure::Constraint(format!("A node carries at most {ceiling} labels"))
        }
        graph_engine::GraphWriteError::NameTooLong { size, ceiling } => {
            QueryFailure::Constraint(format!(
                "An id, a label and a relationship type are at most {ceiling} bytes, and this \
                 one is {size}"
            ))
        }
        graph_engine::GraphWriteError::DuplicateId { id } => {
            QueryFailure::Constraint(format!("An element with the id {id} already exists"))
        }
        graph_engine::GraphWriteError::MissingEndpoint { id } => QueryFailure::Constraint(format!(
            "A relationship joins two nodes this graph holds, and {id} is not one"
        )),
    }
}

#[cfg(test)]
mod tests;
