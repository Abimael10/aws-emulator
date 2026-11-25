// SPDX-License-Identifier: CC0-1.0

//! Evaluating a Gremlin traversal against the property graph this engine stores, and the
//! GraphSON the Gremlin endpoint answers with.
//!
//! The Gremlin query endpoint runs the traversal it was handed, so every value below is
//! read out of, or written into, the ONE graph this engine holds
//! ([`super::PropertyGraph`] — the same store the openCypher endpoints read and write):
//! the elements a step reaches, the properties it reports and the count it answers are
//! what the store holds at the instant the traversal ran, never anything derived from the
//! query text.
//!
//! How a traversal is evaluated. The traversal is read first (see
//! [`super::parse_gremlin_traversal`]) into the steps it was written as; this module then
//! runs those steps in order over a STREAM of traversers, as Apache TinkerPop evaluates
//! one: each step is handed the stream the step before it produced, and the last stream is
//! what the endpoint answers with. A step this engine does not evaluate yet stops the run
//! and is reported as exactly that ([`TraversalFailure::NotEvaluated`]) — never as an
//! empty answer, which a caller cannot tell from a traversal that really matched nothing.
//!
//! What bounds one run: the work budget, the descent and the deadline of
//! [`super::Budget`], which the openCypher evaluation is bounded by too, so one query
//! endpoint cannot be made to spend more than the other.
//!
//! A traversal that WRITES (`addV`, `addE`, `property`, `drop`) runs against a copy of the
//! graph and replaces the stored one only once the whole traversal has succeeded, so a
//! traversal that fails part-way leaves the graph as it was.
//!
//! WHY THE STEPS ARE APPLIED HERE. What a caller submits is not a program: a traversal is a
//! chain of steps named by the query language's own fixed vocabulary (Apache TinkerPop's
//! `GraphTraversal`, `P` and `TextP`) applied to literal arguments, and it is READ by the
//! reader this crate already has ([`super::parse_gremlin_traversal`]) before anything is
//! evaluated. No maintained Rust crate evaluates a TinkerPop traversal in process — the
//! published Gremlin crates are CLIENTS that dial a remote Gremlin Server, which this
//! emulator never does — so the steps are applied here, and ONLY the steps named below:
//! every other name stops the run, so nothing can be approximated. The evaluation's whole
//! world is the graph it is handed and the budget it is given: it opens no socket, starts no
//! process or thread, reads no file and no environment variable, and loads nothing, so no
//! step has a host primitive to reach even if one named it. The one allocation a step may
//! grow, and the time it may spend, are charged against that budget before the work.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use ::serde_json::Value;

use crate::ops::types::{
    ConstraintViolationException, MemoryLimitExceededException, TimeLimitExceededException,
};

use super::gremlin_plan::{
    is_source_configuration, Argument, Call, Traversal, PREDICATES, TRAVERSAL_ACTIONS,
};
use super::open_cypher::{
    write_refusal_reason, Budget, QueryFailure, QueryRefusal, WriteCeiling, MAX_QUERY_RESULT_ROWS,
    MAX_QUERY_WORK_UNITS, MAX_RESULT_CHARS,
};
use super::property_graph::PropertyGraph;

/// The steps that CHANGE the graph, which is what makes a traversal run against a copy of
/// it.
///
/// Source: the mutating steps of Apache TinkerPop's `GraphTraversal` this engine
/// evaluates — the two that add an element, the one that writes a property and the one
/// that removes an element.
pub const WRITING_STEPS: &[&str] = &["addV", "addE", "property", "drop"];

/// The label a vertex created without one carries.
///
/// Source: Apache TinkerPop's `Vertex.DEFAULT_LABEL`, which is what `addV()` with no
/// argument labels the vertex it adds.
pub const DEFAULT_VERTEX_LABEL: &str = "vertex";

/// How Neptune reports a node carrying SEVERAL labels as one Gremlin label: the labels
/// joined by `::`, which is the separator the engine's property-graph/openCypher
/// interoperability uses for a multi-labelled node.
pub const LABEL_SEPARATOR: &str = "::";

/// Why a traversal this engine READ could not be answered.
///
/// Each variant is a different fact about the request, so each is reported through its own
/// modelled error ([`traversal_refusal`]).
#[derive(Debug)]
pub enum TraversalFailure {
    /// The traversal names a step this engine does not evaluate yet. NOT a refusal of the
    /// request: the traversal is a valid one, and answering it with an empty result or
    /// with another error class would claim something that is not so.
    NotEvaluated { step: String },
    /// The traversal cannot be evaluated as written: a step applied to a value it does not
    /// take, or an argument that is not one the step accepts.
    Refused(String),
    /// A bound the run met, or a write the graph would not take.
    Run(QueryFailure),
}

impl From<QueryFailure> for TraversalFailure {
    fn from(failure: QueryFailure) -> Self {
        Self::Run(failure)
    }
}

/// What the endpoint answers one failure with: a modelled refusal, or the statement that
/// the step is not evaluated here yet.
#[derive(Debug)]
pub enum TraversalRefusal {
    /// A step this engine does not evaluate: the text states which, so a caller can tell
    /// an unwritten step from a request of its own that was wrong.
    NotEvaluated(String),
    /// A modelled refusal, with its payload.
    Modeled(QueryRefusal),
}

/// The refusal `failure` is reported through.
///
/// A traversal that PARSED and then cannot be applied — a step handed a result that is not
/// an element, an argument that is not the kind the step takes — is reported as
/// `MalformedQueryException`, the error whose own documentation covers a query that "is
/// syntactically incorrect or does not pass additional validation"
/// (smithy_query(query="shape", shape="MalformedQueryException")): applying the steps to
/// one another is that additional validation, and the message states which step and what
/// it was applied to.
///
/// Every bound and every write refusal is worded the way the GREMLIN endpoint words one —
/// the payloads are this module's and `super::gremlin_plan`'s — so a Gremlin caller is
/// never told about the other query language.
#[must_use]
pub fn traversal_refusal(failure: TraversalFailure) -> TraversalRefusal {
    match failure {
        TraversalFailure::NotEvaluated { step } => TraversalRefusal::NotEvaluated(format!(
            "the Gremlin step {step}() is not evaluated by this emulator yet"
        )),
        TraversalFailure::Refused(reason) => {
            TraversalRefusal::Modeled(QueryRefusal::Malformed(super::malformed_gremlin_query(
                &reason,
            )))
        }
        TraversalFailure::Run(failure) => TraversalRefusal::Modeled(run_refusal(failure)),
    }
}

/// The modelled refusal a bound met, or a write the graph refused, is reported through.
fn run_refusal(failure: QueryFailure) -> QueryRefusal {
    match failure {
        QueryFailure::Malformed { reason } | QueryFailure::Refused(reason) => {
            QueryRefusal::Malformed(super::malformed_gremlin_query(&reason))
        }
        QueryFailure::Unsupported { clause } => {
            QueryRefusal::Malformed(super::malformed_gremlin_query(clause))
        }
        QueryFailure::Constraint(reason) => QueryRefusal::Constraint(gremlin_constraint(&reason)),
        QueryFailure::MissingParameter(name) => QueryRefusal::Malformed(
            super::malformed_gremlin_query(&format!("the query references {name}, which it never binds")),
        ),
        QueryFailure::TooLong {
            characters,
            ceiling,
        } => QueryRefusal::TooLarge(super::gremlin_query_too_large(&format!(
            "the query is {characters} characters, and this engine reads at most {ceiling}"
        ))),
        QueryFailure::TooDeep { depth, ceiling } => {
            QueryRefusal::Limit(super::gremlin_query_too_complex(&format!(
                "the query nests {depth} deep, and this engine reads at most {ceiling}"
            )))
        }
        QueryFailure::TooLarge { rows, ceiling } => {
            QueryRefusal::Limit(super::gremlin_query_too_complex(&format!(
                "the traversal answers {rows} results, and this engine answers at most {ceiling}"
            )))
        }
        QueryFailure::ValueTooLarge { size, ceiling } => {
            QueryRefusal::Limit(super::gremlin_query_too_complex(&format!(
                "the traversal builds a value of {size}, and this engine builds at most {ceiling}"
            )))
        }
        QueryFailure::TooNested { ceiling } => {
            QueryRefusal::Limit(super::gremlin_query_too_complex(&format!(
                "the traversal evaluates deeper than the {ceiling} levels one query may descend"
            )))
        }
        QueryFailure::TooManyHops { asked, ceiling } => {
            QueryRefusal::Limit(super::gremlin_query_too_complex(&format!(
                "the traversal walks up to {asked} hops, and this engine walks at most {ceiling}"
            )))
        }
        QueryFailure::Exhausted { ceiling } => {
            QueryRefusal::Limit(super::gremlin_query_too_complex(&format!(
                "the traversal asked for more than the {ceiling} units of work one query may spend"
            )))
        }
        QueryFailure::TimedOut { ceiling } => {
            QueryRefusal::TimeLimit(gremlin_time_limit(&format!(
                "one query runs for at most {} seconds",
                ceiling.as_secs()
            )))
        }
        QueryFailure::Write(refused) => {
            let (ceiling, reason) = write_refusal_reason(&refused);
            match ceiling {
                WriteCeiling::Room => QueryRefusal::Memory(gremlin_memory_limit(&reason)),
                WriteCeiling::Rule => QueryRefusal::Constraint(gremlin_constraint(&reason)),
            }
        }
        QueryFailure::Interrupted(reason) => QueryRefusal::Failure(super::engine_failure(&reason)),
    }
}

/// The `TimeLimitExceededException` payload for a traversal whose span ran out.
#[must_use]
pub fn gremlin_time_limit(reason: &str) -> TimeLimitExceededException {
    TimeLimitExceededException {
        detailed_message: format!("Gremlin query exceeded the time limit: {reason}"),
        request_id: super::new_request_id(),
    }
}

/// The `MemoryLimitExceededException` payload for a write the store had no room for.
#[must_use]
pub fn gremlin_memory_limit(reason: &str) -> MemoryLimitExceededException {
    MemoryLimitExceededException {
        detailed_message: format!("Gremlin query ran out of room: {reason}"),
        request_id: super::new_request_id(),
    }
}

/// The `ConstraintViolationException` payload for a write the graph's own rules refuse.
#[must_use]
pub fn gremlin_constraint(reason: &str) -> ConstraintViolationException {
    ConstraintViolationException {
        detailed_message: reason.to_string(),
        request_id: super::new_request_id(),
    }
}

/// One traverser's value: an element of the graph, or data a step computed from one.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    /// The node an id names.
    Vertex(String),
    /// The relationship an id names.
    Edge(String),
    /// A whole number a step counted, which GraphSON types as a 64-bit integer.
    Count(i64),
    /// A value, a list or a map a step read or built.
    Data(Value),
}

/// The results `traversal` answers with, as GraphSON, after applying whatever it writes to
/// `graph`.
///
/// `deadline` is the instant the run must stop at. A traversal that writes is applied to a
/// copy of the graph and replaces the stored one only once the whole traversal has
/// succeeded, so a traversal that fails part-way changes nothing.
pub fn run_traversal(
    graph: &mut PropertyGraph,
    traversal: &Traversal,
    deadline: Instant,
) -> Result<Vec<Value>, TraversalFailure> {
    let writes = traversal
        .steps
        .iter()
        .any(|step| WRITING_STEPS.contains(&step.name.as_str()));
    // The copy is what makes the whole traversal atomic: it is written to, and only a
    // traversal that finished replaces the stored graph. A traversal that writes nothing
    // needs no copy, so a read is handed the stored graph itself.
    let mut working = writes.then(|| graph.clone());
    let answered = {
        let target: &mut PropertyGraph = match working.as_mut() {
            Some(copy) => copy,
            None => graph,
        };
        let mut run = Run {
            graph: target,
            budget: Budget::of(MAX_QUERY_WORK_UNITS, deadline),
        };
        match run.evaluate(traversal) {
            Ok(stream) => run.rendered(&stream),
            Err(failure) => Err(failure),
        }
    };
    let rendered = answered?;
    if let Some(written) = working {
        *graph = written;
    }
    Ok(rendered)
}

/// One run of one traversal: the graph it reads and writes, and what it may spend.
struct Run<'graph> {
    graph: &'graph mut PropertyGraph,
    budget: Budget,
}

impl Run<'_> {
    /// The stream the last step of `traversal` produced.
    fn evaluate(&mut self, traversal: &Traversal) -> Result<Vec<Item>, TraversalFailure> {
        let mut stream: Vec<Item> = Vec::new();
        // Whether a step has reached the graph yet. A traversal is read from its source
        // onwards, so a step that maps or filters elements has nothing to work on until
        // one has.
        let mut sourced = false;
        let mut index = 0;
        while index < traversal.steps.len() {
            let step = &traversal.steps[index];
            index += 1;
            let name = step.name.as_str();
            self.budget.charge(1)?;
            // A step that configures the traversal SOURCE adds nothing to the traversal,
            // so nothing is evaluated for it.
            if is_source_configuration(name) {
                continue;
            }
            if TRAVERSAL_ACTIONS.contains(&name) {
                stream = self.action(name, stream)?;
                continue;
            }
            stream = match name {
                "V" => {
                    sourced = true;
                    self.vertices(step)?
                }
                "E" => {
                    sourced = true;
                    self.edges(step)?
                }
                "addV" => {
                    sourced = true;
                    self.add_vertex(step)?
                }
                "addE" => {
                    // `from` and `to` configure the `addE` they follow rather than adding
                    // a step of their own, so they are read here with it.
                    let mut endpoints: Vec<&Call> = Vec::new();
                    while let Some(next) = traversal.steps.get(index) {
                        if next.name == "from" || next.name == "to" {
                            endpoints.push(next);
                            index += 1;
                        } else {
                            break;
                        }
                    }
                    let added = self.add_edge(step, &endpoints, &stream)?;
                    sourced = true;
                    added
                }
                _ if !sourced => {
                    return Err(TraversalFailure::Refused(format!(
                        "The step {name}() has no elements to work on: a traversal reaches \
                         the graph through V(), E(), addV() or addE() first"
                    )))
                }
                "has" => self.has(step, stream)?,
                "hasLabel" => self.has_label(step, stream)?,
                "hasId" => self.has_id(step, stream)?,
                "hasNot" => self.has_not(step, stream)?,
                "hasKey" => self.has_key(step, stream)?,
                "out" | "in" | "both" => self.adjacent(name, step, stream)?,
                "outE" | "inE" | "bothE" => self.incident(name, step, stream)?,
                "outV" | "inV" | "bothV" | "otherV" => self.endpoints(name, stream)?,
                "values" => self.values(step, stream)?,
                "valueMap" => self.value_map(step, stream)?,
                "id" => self.ids(stream)?,
                "label" => self.labels(stream)?,
                "count" => vec![Item::Count(i64::try_from(stream.len()).unwrap_or(i64::MAX))],
                "limit" => self.limited(step, stream)?,
                "dedup" => deduplicated(&self.budget, stream)?,
                "property" => self.write_property(step, stream)?,
                "drop" => self.drop_elements(stream)?,
                // A modulator or a step label configures the step it follows, and this
                // engine evaluates neither yet; so does every step not named above.
                _ => {
                    return Err(TraversalFailure::NotEvaluated {
                        step: name.to_string(),
                    })
                }
            };
            if stream.len() > MAX_QUERY_RESULT_ROWS {
                return Err(QueryFailure::TooLarge {
                    rows: stream.len(),
                    ceiling: MAX_QUERY_RESULT_ROWS,
                }
                .into());
            }
        }
        Ok(stream)
    }

    /// What a traversal ACTION asks of the finished traversal.
    fn action(&mut self, name: &str, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        match name {
            // The next result, or none when the traversal produced none.
            "next" | "tryNext" => Ok(stream.into_iter().take(1).collect()),
            "hasNext" => Ok(vec![Item::Data(Value::Bool(!stream.is_empty()))]),
            "toSet" | "toBulkSet" => deduplicated(&self.budget, stream),
            "toList" | "toStream" | "fill" => Ok(stream),
            // The traversal is driven to exhaustion for its side effects, so it answers
            // with no result at all.
            "iterate" => Ok(Vec::new()),
            // `explain` and `profile` are what the explain and profile ENDPOINTS answer;
            // asking the query endpoint for one is a different report than it serves.
            _ => Err(TraversalFailure::NotEvaluated {
                step: name.to_string(),
            }),
        }
    }

    /// `V()`: every node the store holds, or the ones the ids name.
    fn vertices(&mut self, step: &Call) -> Result<Vec<Item>, TraversalFailure> {
        let asked = self.element_ids(step)?;
        let mut stream = Vec::new();
        if asked.is_empty() {
            for id in self.graph.node_ids() {
                self.budget.charge(1)?;
                stream.push(Item::Vertex(id));
            }
            return Ok(stream);
        }
        for id in asked {
            self.budget.charge(1)?;
            if self.graph.node(&id).is_some() {
                stream.push(Item::Vertex(id));
            }
        }
        Ok(stream)
    }

    /// `E()`: every relationship the store holds, or the ones the ids name.
    fn edges(&mut self, step: &Call) -> Result<Vec<Item>, TraversalFailure> {
        let asked = self.element_ids(step)?;
        let mut stream = Vec::new();
        if asked.is_empty() {
            for id in self.graph.relationship_ids() {
                self.budget.charge(1)?;
                stream.push(Item::Edge(id));
            }
            return Ok(stream);
        }
        for id in asked {
            self.budget.charge(1)?;
            if self.graph.relationship(&id).is_some() {
                stream.push(Item::Edge(id));
            }
        }
        Ok(stream)
    }

    /// The element ids a step names, as the store addresses one: a string.
    fn element_ids(&self, step: &Call) -> Result<Vec<String>, TraversalFailure> {
        let mut ids = Vec::new();
        for argument in step.arguments() {
            match argument {
                Argument::Text(id) => ids.push(id.clone()),
                Argument::Number(number) => ids.push(number.clone()),
                Argument::List(items) => {
                    for item in items {
                        match item {
                            Argument::Text(id) => ids.push(id.clone()),
                            Argument::Number(number) => ids.push(number.clone()),
                            _ => return Err(not_an_id(&step.name)),
                        }
                    }
                }
                _ => return Err(not_an_id(&step.name)),
            }
        }
        Ok(ids)
    }

    /// `addV(label?)`: a node carrying the label the step names, or the default one.
    fn add_vertex(&mut self, step: &Call) -> Result<Vec<Item>, TraversalFailure> {
        let label = match step.arguments() {
            [] => DEFAULT_VERTEX_LABEL.to_string(),
            [Argument::Text(label)] => label.clone(),
            _ => {
                return Err(TraversalFailure::Refused(
                    "addV() takes the label of the vertex it adds, as a string".to_string(),
                ))
            }
        };
        self.budget.charge(1)?;
        let id = self
            .graph
            .insert_node(None, vec![label], BTreeMap::new())
            .map_err(QueryFailure::Write)?;
        Ok(vec![Item::Vertex(id)])
    }

    /// `addE(label)` with its `from`/`to`: a relationship between the nodes they name, or
    /// between the node one of them names and each node the stream carries.
    fn add_edge(
        &mut self,
        step: &Call,
        endpoints: &[&Call],
        stream: &[Item],
    ) -> Result<Vec<Item>, TraversalFailure> {
        let [Argument::Text(label)] = step.arguments() else {
            return Err(TraversalFailure::Refused(
                "addE() takes the label of the edge it adds, as a string".to_string(),
            ));
        };
        let label = label.clone();
        let mut leaves = None;
        let mut enters = None;
        for endpoint in endpoints {
            let id = self.endpoint_id(endpoint)?;
            if endpoint.name == "from" {
                leaves = Some(id);
            } else {
                enters = Some(id);
            }
        }
        let joined: Vec<(String, String)> = match (leaves, enters) {
            (Some(from), Some(to)) => vec![(from, to)],
            (Some(from), None) => stream
                .iter()
                .map(|item| Ok((from.clone(), vertex_of(item)?)))
                .collect::<Result<_, TraversalFailure>>()?,
            (None, Some(to)) => stream
                .iter()
                .map(|item| Ok((vertex_of(item)?, to.clone())))
                .collect::<Result<_, TraversalFailure>>()?,
            (None, None) => {
                return Err(TraversalFailure::Refused(
                    "addE() joins two nodes, which from() and to() name".to_string(),
                ))
            }
        };
        let mut added = Vec::new();
        for (from, to) in joined {
            self.budget.charge(1)?;
            let id = self
                .graph
                .insert_relationship(None, label.clone(), &from, &to, BTreeMap::new())
                .map_err(QueryFailure::Write)?;
            added.push(Item::Edge(id));
        }
        Ok(added)
    }

    /// The node a `from`/`to` names: an id, or the one an anonymous `V(id)` traversal
    /// reaches.
    fn endpoint_id(&self, modulator: &Call) -> Result<String, TraversalFailure> {
        match modulator.arguments() {
            [Argument::Text(id)] => Ok(id.clone()),
            [Argument::Expression(chain)] => chain
                .iter()
                .find(|call| call.name == "V")
                .and_then(|call| match call.arguments() {
                    [Argument::Text(id)] => Some(id.clone()),
                    _ => None,
                })
                .ok_or_else(|| TraversalFailure::NotEvaluated {
                    step: modulator.name.clone(),
                }),
            _ => Err(TraversalFailure::Refused(format!(
                "{}() names the node the edge joins, by id or with V(id)",
                modulator.name
            ))),
        }
    }

    /// `has(...)`: the elements whose property satisfies what the step names.
    fn has(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let arguments = step.arguments();
        // `has(label, key, value)` narrows by label first, which is the three-argument
        // form; the two-argument one is the property alone, and the one-argument one asks
        // only that the property is there.
        let (label, key, test) = match arguments {
            [Argument::Text(key)] => (None, key.clone(), None),
            [Argument::Text(key), test] => (None, key.clone(), Some(test)),
            [Argument::Text(label), Argument::Text(key), test] => {
                (Some(label.clone()), key.clone(), Some(test))
            }
            _ => {
                return Err(TraversalFailure::Refused(
                    "has() takes a property key, and the value or predicate it must satisfy"
                        .to_string(),
                ))
            }
        };
        let predicate = match test {
            Some(test) => Some(self.predicate(test)?),
            None => None,
        };
        let mut kept = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            applied_to_element(&item, "has")?;
            if let Some(label) = &label {
                if !self.carries_label(&item, label) {
                    continue;
                }
            }
            let held = self.property(&item, &key);
            let matched = match (&predicate, held) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(predicate), Some(value)) => predicate.admits(&value),
            };
            if matched {
                kept.push(item);
            }
        }
        Ok(kept)
    }

    /// `hasLabel(...)`: the elements carrying one of the labels the step names.
    fn has_label(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let wanted = self.text_arguments(step)?;
        let mut kept = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            applied_to_element(&item, "hasLabel")?;
            if wanted.iter().any(|label| self.carries_label(&item, label)) {
                kept.push(item);
            }
        }
        Ok(kept)
    }

    /// `hasId(...)`: the elements the ids name.
    fn has_id(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let wanted = self.element_ids(step)?;
        let mut kept = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            let id = match &item {
                Item::Vertex(id) | Item::Edge(id) => id.clone(),
                _ => return Err(not_an_element("hasId")),
            };
            if wanted.contains(&id) {
                kept.push(item);
            }
        }
        Ok(kept)
    }

    /// `hasNot(key)`: the elements carrying no property of that key.
    fn has_not(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let keys = self.text_arguments(step)?;
        let mut kept = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            applied_to_element(&item, "hasNot")?;
            if keys.iter().all(|key| self.property(&item, key).is_none()) {
                kept.push(item);
            }
        }
        Ok(kept)
    }

    /// `hasKey(key)`: the elements carrying a property of that key.
    fn has_key(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let keys = self.text_arguments(step)?;
        let mut kept = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            applied_to_element(&item, "hasKey")?;
            if keys.iter().any(|key| self.property(&item, key).is_some()) {
                kept.push(item);
            }
        }
        Ok(kept)
    }

    /// `out`/`in`/`both`: the nodes the relationships of the named types reach.
    fn adjacent(
        &mut self,
        name: &str,
        step: &Call,
        stream: Vec<Item>,
    ) -> Result<Vec<Item>, TraversalFailure> {
        let types = self.text_arguments(step)?;
        let mut reached = Vec::new();
        for item in stream {
            let from = vertex_of(&item)?;
            for id in self.graph.incident_relationship_ids(&from) {
                self.budget.charge(1)?;
                let Some(relationship) = self.graph.relationship(&id) else {
                    continue;
                };
                if !types.is_empty() && !types.contains(&relationship.rel_type) {
                    continue;
                }
                let leaves = relationship.from_node == from;
                let enters = relationship.to_node == from;
                let other = if leaves {
                    relationship.to_node.clone()
                } else {
                    relationship.from_node.clone()
                };
                let take = match name {
                    "out" => leaves,
                    "in" => enters,
                    _ => leaves || enters,
                };
                if !take {
                    continue;
                }
                if name == "both" && leaves && enters {
                    // A self-loop is reached from both of its ends.
                    reached.push(Item::Vertex(other.clone()));
                }
                reached.push(Item::Vertex(other));
            }
        }
        Ok(reached)
    }

    /// `outE`/`inE`/`bothE`: the relationships of the named types at each node.
    fn incident(
        &mut self,
        name: &str,
        step: &Call,
        stream: Vec<Item>,
    ) -> Result<Vec<Item>, TraversalFailure> {
        let types = self.text_arguments(step)?;
        let mut reached = Vec::new();
        for item in stream {
            let at = vertex_of(&item)?;
            for id in self.graph.incident_relationship_ids(&at) {
                self.budget.charge(1)?;
                let Some(relationship) = self.graph.relationship(&id) else {
                    continue;
                };
                if !types.is_empty() && !types.contains(&relationship.rel_type) {
                    continue;
                }
                let take = match name {
                    "outE" => relationship.from_node == at,
                    "inE" => relationship.to_node == at,
                    _ => true,
                };
                if take {
                    reached.push(Item::Edge(id));
                }
            }
        }
        Ok(reached)
    }

    /// `outV`/`inV`/`bothV`/`otherV`: the nodes a relationship joins.
    fn endpoints(&mut self, name: &str, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let mut reached = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            let Item::Edge(id) = &item else {
                return Err(not_an_element(name));
            };
            let Some(relationship) = self.graph.relationship(id) else {
                continue;
            };
            match name {
                "outV" => reached.push(Item::Vertex(relationship.from_node.clone())),
                "inV" => reached.push(Item::Vertex(relationship.to_node.clone())),
                // `otherV` is the end that is not the one the traversal came from, which
                // this engine does not track, so it is not evaluated here.
                "otherV" => {
                    return Err(TraversalFailure::NotEvaluated {
                        step: name.to_string(),
                    })
                }
                _ => {
                    reached.push(Item::Vertex(relationship.from_node.clone()));
                    reached.push(Item::Vertex(relationship.to_node.clone()));
                }
            }
        }
        Ok(reached)
    }

    /// `values(keys...)`: each value of the named properties, or of every property the
    /// element carries when the step names none.
    fn values(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let keys = self.text_arguments(step)?;
        let mut read = Vec::new();
        for item in stream {
            let held = self.properties(&item)?;
            let asked: Vec<String> = if keys.is_empty() {
                held.keys().cloned().collect()
            } else {
                keys.clone()
            };
            for key in asked {
                self.budget.charge(1)?;
                let Some(value) = held.get(&key) else {
                    continue;
                };
                // A property holding several values answers one result per value, as a
                // multi-properties step does.
                match value {
                    Value::Array(items) => {
                        for value in items {
                            self.budget.charge(1)?;
                            read.push(Item::Data(value.clone()));
                        }
                    }
                    value => read.push(Item::Data(value.clone())),
                }
            }
        }
        Ok(read)
    }

    /// `valueMap(keys...)`: each element's properties, by key, each key's values as a
    /// list — the form a value map carries them in.
    fn value_map(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let keys = self.text_arguments(step)?;
        let mut read = Vec::new();
        for item in stream {
            let held = self.properties(&item)?;
            let mut mapped = ::serde_json::Map::new();
            for (key, value) in &held {
                self.budget.charge(1)?;
                if !keys.is_empty() && !keys.iter().any(|asked| asked == key) {
                    continue;
                }
                let values = match value {
                    Value::Array(items) => items.clone(),
                    value => vec![value.clone()],
                };
                mapped.insert(key.clone(), Value::Array(values));
            }
            read.push(Item::Data(Value::Object(mapped)));
        }
        Ok(read)
    }

    /// `id()`: the id each element is addressed by.
    fn ids(&mut self, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let mut read = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            match item {
                Item::Vertex(id) | Item::Edge(id) => read.push(Item::Data(Value::String(id))),
                _ => return Err(not_an_element("id")),
            }
        }
        Ok(read)
    }

    /// `label()`: the label of each element.
    fn labels(&mut self, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let mut read = Vec::new();
        for item in stream {
            self.budget.charge(1)?;
            let label = self
                .label_of(&item)
                .ok_or_else(|| not_an_element("label"))?;
            read.push(Item::Data(Value::String(label)));
        }
        Ok(read)
    }

    /// `limit(n)`: the first `n` results of the stream.
    fn limited(&mut self, step: &Call, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        let [Argument::Number(number)] = step.arguments() else {
            return Err(TraversalFailure::Refused(
                "limit() takes how many results to keep, as a number".to_string(),
            ));
        };
        let kept: i64 = number.parse().map_err(|_| {
            TraversalFailure::Refused(format!(
                "limit() takes a whole number of results, and {} is not one",
                super::echoed(number)
            ))
        })?;
        let kept = usize::try_from(kept).map_err(|_| {
            TraversalFailure::Refused(
                "limit() keeps a number of results that cannot be negative".to_string(),
            )
        })?;
        Ok(stream.into_iter().take(kept).collect())
    }

    /// `property(key, value)`: the value written on each element of the stream, which the
    /// step hands on.
    fn write_property(
        &mut self,
        step: &Call,
        stream: Vec<Item>,
    ) -> Result<Vec<Item>, TraversalFailure> {
        // The cardinality form (`property(single, key, value)`) names how a value joins
        // the ones already there; `single` replaces them, which is what this engine
        // writes, and the others are not evaluated here.
        let (key, written) = match step.arguments() {
            [Argument::Text(key), value] => (key.clone(), literal(value)?),
            [Argument::Expression(cardinality), Argument::Text(key), value] => {
                let named = cardinality
                    .first()
                    .map(|call| call.name.as_str())
                    .unwrap_or_default();
                if !named.eq_ignore_ascii_case("single") {
                    return Err(TraversalFailure::NotEvaluated {
                        step: format!("property({named}, …)"),
                    });
                }
                (key.clone(), literal(value)?)
            }
            _ => {
                return Err(TraversalFailure::Refused(
                    "property() takes the key to write and the value to write there"
                        .to_string(),
                ))
            }
        };
        for item in &stream {
            self.budget.charge(1)?;
            let id = match item {
                Item::Vertex(id) | Item::Edge(id) => id.clone(),
                _ => return Err(not_an_element("property")),
            };
            self.graph
                .set_element_property(&id, &key, Some(written.clone()))
                .map_err(QueryFailure::Write)?;
        }
        Ok(stream)
    }

    /// `drop()`: every element of the stream removed from the graph, and nothing answered.
    ///
    /// Removing a node removes the relationships at it too: a relationship joins two nodes
    /// the graph holds, so one left behind would name a node that is gone.
    fn drop_elements(&mut self, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
        for item in &stream {
            self.budget.charge(1)?;
            match item {
                Item::Vertex(id) => {
                    for relationship in self.graph.incident_relationship_ids(id) {
                        self.budget.charge(1)?;
                        self.graph.remove_relationship(&relationship);
                    }
                    self.graph.remove_node(id);
                }
                Item::Edge(id) => {
                    self.graph.remove_relationship(id);
                }
                _ => return Err(not_an_element("drop")),
            }
        }
        Ok(Vec::new())
    }

    /// The text arguments a step was applied to.
    fn text_arguments(&self, step: &Call) -> Result<Vec<String>, TraversalFailure> {
        let mut texts = Vec::new();
        for argument in step.arguments() {
            match argument {
                Argument::Text(text) => texts.push(text.clone()),
                Argument::List(items) => {
                    for item in items {
                        match item {
                            Argument::Text(text) => texts.push(text.clone()),
                            _ => return Err(not_a_name(&step.name)),
                        }
                    }
                }
                _ => return Err(not_a_name(&step.name)),
            }
        }
        Ok(texts)
    }

    /// What a `has` compares a property against: the predicate the argument names, or
    /// equality with the value it is.
    fn predicate(&self, argument: &Argument) -> Result<Predicate, TraversalFailure> {
        let Argument::Expression(chain) = argument else {
            return Ok(Predicate::Equal(literal(argument)?));
        };
        // A predicate is written either on its own (`gt(30)`) or qualified by the class it
        // belongs to (`P.gt(30)`, `TextP.containing('AN')`), which names the same
        // predicate.
        let call = match chain.as_slice() {
            [call] => call,
            [qualifier, call]
                if matches!(qualifier.name.as_str(), "P" | "TextP")
                    && qualifier.arguments.is_none() =>
            {
                call
            }
            _ => {
                return Err(TraversalFailure::NotEvaluated {
                    step: "has(…)".to_string(),
                })
            }
        };
        let name = call.name.as_str();
        if !PREDICATES.contains(&name) {
            return Err(TraversalFailure::NotEvaluated {
                step: name.to_string(),
            });
        }
        let arguments = call.arguments();
        let one = |index: usize| -> Result<Value, TraversalFailure> {
            arguments
                .get(index)
                .ok_or_else(|| {
                    TraversalFailure::Refused(format!("{name}() takes the value it compares to"))
                })
                .and_then(literal)
        };
        let many = || -> Result<Vec<Value>, TraversalFailure> {
            let mut values = Vec::new();
            for argument in arguments {
                match argument {
                    Argument::List(items) => {
                        for item in items {
                            values.push(literal(item)?);
                        }
                    }
                    argument => values.push(literal(argument)?),
                }
            }
            Ok(values)
        };
        let text = |index: usize| -> Result<String, TraversalFailure> {
            match one(index)? {
                Value::String(text) => Ok(text),
                _ => Err(TraversalFailure::Refused(format!(
                    "{name}() compares text, so it takes a string"
                ))),
            }
        };
        match name {
            "eq" => Ok(Predicate::Equal(one(0)?)),
            "neq" => Ok(Predicate::NotEqual(one(0)?)),
            "lt" => Ok(Predicate::Less(one(0)?)),
            "lte" => Ok(Predicate::AtMost(one(0)?)),
            "gt" => Ok(Predicate::Greater(one(0)?)),
            "gte" => Ok(Predicate::AtLeast(one(0)?)),
            "between" => Ok(Predicate::Between(one(0)?, one(1)?)),
            "inside" => Ok(Predicate::Inside(one(0)?, one(1)?)),
            "outside" => Ok(Predicate::Outside(one(0)?, one(1)?)),
            "within" => Ok(Predicate::Within(many()?)),
            "without" => Ok(Predicate::Without(many()?)),
            "containing" => Ok(Predicate::Containing(text(0)?)),
            "notContaining" => Ok(Predicate::NotContaining(text(0)?)),
            "startingWith" => Ok(Predicate::StartingWith(text(0)?)),
            "notStartingWith" => Ok(Predicate::NotStartingWith(text(0)?)),
            "endingWith" => Ok(Predicate::EndingWith(text(0)?)),
            "notEndingWith" => Ok(Predicate::NotEndingWith(text(0)?)),
            _ => Err(TraversalFailure::NotEvaluated {
                step: name.to_string(),
            }),
        }
    }

    /// Whether the element `item` names carries `label`.
    fn carries_label(&self, item: &Item, label: &str) -> bool {
        match item {
            Item::Vertex(id) => self.graph.node(id).is_some_and(|node| {
                node.labels.iter().any(|held| held == label)
                    || node.labels.join(LABEL_SEPARATOR) == label
            }),
            Item::Edge(id) => self
                .graph
                .relationship(id)
                .is_some_and(|relationship| relationship.rel_type == label),
            Item::Count(_) | Item::Data(_) => false,
        }
    }

    /// The label the element `item` names reports.
    fn label_of(&self, item: &Item) -> Option<String> {
        match item {
            Item::Vertex(id) => self.graph.node(id).map(|node| {
                if node.labels.is_empty() {
                    DEFAULT_VERTEX_LABEL.to_string()
                } else {
                    node.labels.join(LABEL_SEPARATOR)
                }
            }),
            Item::Edge(id) => self
                .graph
                .relationship(id)
                .map(|relationship| relationship.rel_type.clone()),
            Item::Count(_) | Item::Data(_) => None,
        }
    }

    /// Every property the element `item` names carries.
    fn properties(&self, item: &Item) -> Result<BTreeMap<String, Value>, TraversalFailure> {
        match item {
            Item::Vertex(id) => Ok(self
                .graph
                .node(id)
                .map(|node| node.properties.clone())
                .unwrap_or_default()),
            Item::Edge(id) => Ok(self
                .graph
                .relationship(id)
                .map(|relationship| relationship.properties.clone())
                .unwrap_or_default()),
            Item::Count(_) | Item::Data(_) => Err(not_an_element("values")),
        }
    }

    /// The value of one property of the element `item` names.
    fn property(&self, item: &Item, key: &str) -> Option<Value> {
        match item {
            Item::Vertex(id) => self
                .graph
                .node(id)
                .and_then(|node| node.properties.get(key).cloned()),
            Item::Edge(id) => self
                .graph
                .relationship(id)
                .and_then(|relationship| relationship.properties.get(key).cloned()),
            Item::Count(_) | Item::Data(_) => None,
        }
    }

    /// The stream as the Gremlin endpoint answers it: one GraphSON value per result.
    ///
    /// BOUNDED AS IT IS BUILT, so a small request cannot be answered with an arbitrarily
    /// large response and nothing past the ceiling is ever rendered: what ONE element
    /// contributes is charged from what the store holds for it BEFORE that element is
    /// written (so an answer carrying a large element, or carrying the same one many
    /// times, pays for each copy), and the running total of the whole answer is checked
    /// against [`MAX_RESULT_CHARS`] on every result rather than once the vector exists.
    fn rendered(&self, stream: &[Item]) -> Result<Vec<Value>, TraversalFailure> {
        let mut data = Vec::with_capacity(stream.len());
        let mut rendered = 0usize;
        for item in stream {
            self.budget.charge(1)?;
            if let Item::Vertex(id) | Item::Edge(id) = item {
                self.budget
                    .charge_size(self.graph.element_bytes(id), MAX_RESULT_CHARS)?;
            }
            let value = self.graphson(item);
            let size = value.to_string().len();
            self.budget.charge_size(size, MAX_RESULT_CHARS)?;
            rendered = rendered.saturating_add(size);
            // The whole answer, checked with this result counted in, so what is kept
            // never passes the ceiling.
            self.budget.charge_item(rendered, MAX_RESULT_CHARS)?;
            data.push(value);
        }
        Ok(data)
    }

    /// One result as GraphSON 3.0 writes it: a typed value, which is the form the Gremlin
    /// endpoint answers in.
    ///
    /// An ELEMENT is written with the properties the store holds for it as well as its id
    /// and its label — the form every TinkerPop client reads a returned element's
    /// properties off — so a vertex or an edge a traversal answers with carries what is
    /// set on it rather than only its identity. An element carrying no property is written
    /// without the member at all, which is how GraphSON distinguishes it.
    fn graphson(&self, item: &Item) -> Value {
        match item {
            Item::Vertex(id) => {
                let mut value = ::serde_json::json!({
                    "id": id,
                    "label": self.label_of(item).unwrap_or_default(),
                });
                // Read from the stored element rather than copied out of it: the
                // properties are written straight into the answer.
                let written = self
                    .graph
                    .node(id)
                    .and_then(|node| vertex_properties(id, &node.properties));
                if let (Some(members), Some(written)) = (value.as_object_mut(), written) {
                    members.insert(GRAPHSON_PROPERTIES.to_string(), written);
                }
                ::serde_json::json!({
                    GRAPHSON_TYPE: GRAPHSON_VERTEX,
                    GRAPHSON_VALUE: value,
                })
            }
            Item::Edge(id) => {
                let Some(relationship) = self.graph.relationship(id) else {
                    return Value::Null;
                };
                let from = Item::Vertex(relationship.from_node.clone());
                let to = Item::Vertex(relationship.to_node.clone());
                let mut value = ::serde_json::json!({
                    "id": id,
                    "label": relationship.rel_type.clone(),
                    "outV": relationship.from_node.clone(),
                    "outVLabel": self.label_of(&from).unwrap_or_default(),
                    "inV": relationship.to_node.clone(),
                    "inVLabel": self.label_of(&to).unwrap_or_default(),
                });
                if let (Some(members), Some(written)) = (
                    value.as_object_mut(),
                    edge_properties(&relationship.properties),
                ) {
                    members.insert(GRAPHSON_PROPERTIES.to_string(), written);
                }
                ::serde_json::json!({
                    GRAPHSON_TYPE: GRAPHSON_EDGE,
                    GRAPHSON_VALUE: value,
                })
            }
            // A count is a 64-bit integer whatever its value, as the Java type a counting
            // step produces is.
            Item::Count(count) => ::serde_json::json!({
                GRAPHSON_TYPE: GRAPHSON_INT64,
                GRAPHSON_VALUE: count,
            }),
            Item::Data(value) => graphson_value(value),
        }
    }
}

/// The two members every typed GraphSON value carries: which type it is, and the value
/// itself.
const GRAPHSON_TYPE: &str = "@type";
const GRAPHSON_VALUE: &str = "@value";

/// The GraphSON 3.0 type names this endpoint writes.
///
/// Source: the Apache TinkerPop GraphSON 3.0 type system — the `g:` namespace names the
/// graph structure types and the Java number types a value is written as.
const GRAPHSON_VERTEX: &str = "g:Vertex";
const GRAPHSON_EDGE: &str = "g:Edge";
const GRAPHSON_VERTEX_PROPERTY: &str = "g:VertexProperty";
const GRAPHSON_PROPERTY: &str = "g:Property";
const GRAPHSON_LIST: &str = "g:List";
const GRAPHSON_MAP: &str = "g:Map";
const GRAPHSON_INT32: &str = "g:Int32";
const GRAPHSON_INT64: &str = "g:Int64";
const GRAPHSON_DOUBLE: &str = "g:Double";

/// The member an element carries the properties set on it in, and the members one property
/// is written with.
const GRAPHSON_PROPERTIES: &str = "properties";
const GRAPHSON_PROPERTY_KEY: &str = "key";
const GRAPHSON_PROPERTY_LABEL: &str = "label";
const GRAPHSON_PROPERTY_VALUE: &str = "value";
const GRAPHSON_PROPERTY_ID: &str = "id";

/// A vertex's `properties` map as GraphSON 3.0 writes it: keyed by property name, each
/// key's value a LIST of `g:VertexProperty` — one per value the property holds, since a
/// vertex property may be multi-valued — each carrying its own id, its value and the
/// property name as its label.
///
/// `None` when the element carries no property at all, which is the case GraphSON leaves
/// the member out for altogether rather than writing an empty map.
fn vertex_properties(element_id: &str, properties: &BTreeMap<String, Value>) -> Option<Value> {
    if properties.is_empty() {
        return None;
    }
    let mut written = ::serde_json::Map::with_capacity(properties.len());
    for (key, held) in properties {
        // A property holding several values is a multi-property: one `g:VertexProperty`
        // per value, which is how the same property answers one result per value in
        // `values`.
        let values: Vec<&Value> = match held {
            Value::Array(items) => items.iter().collect(),
            value => vec![value],
        };
        let entries: Vec<Value> = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                ::serde_json::json!({
                    GRAPHSON_TYPE: GRAPHSON_VERTEX_PROPERTY,
                    GRAPHSON_VALUE: {
                        GRAPHSON_PROPERTY_ID: typed_integer(
                            i64::from(property_id(element_id, key, index)),
                        ),
                        GRAPHSON_PROPERTY_VALUE: graphson_value(value),
                        GRAPHSON_PROPERTY_LABEL: key,
                    },
                })
            })
            .collect();
        written.insert(key.clone(), Value::Array(entries));
    }
    Some(Value::Object(written))
}

/// An edge's `properties` map as GraphSON 3.0 writes it: keyed by property name, each
/// key's value ONE `g:Property` carrying the key and the value — an edge property is
/// single-valued, so it carries neither an id of its own nor a list.
///
/// `None` for an edge carrying no property, as for a vertex.
fn edge_properties(properties: &BTreeMap<String, Value>) -> Option<Value> {
    if properties.is_empty() {
        return None;
    }
    let mut written = ::serde_json::Map::with_capacity(properties.len());
    for (key, held) in properties {
        written.insert(
            key.clone(),
            ::serde_json::json!({
                GRAPHSON_TYPE: GRAPHSON_PROPERTY,
                GRAPHSON_VALUE: {
                    GRAPHSON_PROPERTY_KEY: key,
                    GRAPHSON_PROPERTY_VALUE: graphson_value(held),
                },
            }),
        );
    }
    Some(Value::Object(written))
}

/// The id one vertex property is reported under: a non-negative 32-bit number this engine
/// derives from the element the property is set on, the property's name and which of the
/// property's values it is.
///
/// Derived rather than stored, so it is the SAME id every read of that property reports —
/// a property a caller reads twice does not change id — and two properties of one element,
/// or the same property name on two elements, do not share one. Nothing the caller sends
/// addresses a property by this id: it is what GraphSON writes in a vertex property's `id`
/// member, and nothing authenticates on it.
///
/// The mixing is the shared one every derived-and-published id in this tree uses
/// (`service_common::stable_hex_id`, SHA-256 from the `sha2` crate); the number is its
/// leading 32 bits with the sign bit cleared, so it is a non-negative value of the type
/// GraphSON writes an id as.
fn property_id(element_id: &str, key: &str, index: usize) -> i32 {
    const HEX_DIGITS_OF_32_BITS: usize = 8;
    let mixed = ::service_common::stable_hex_id(&[element_id, key, &index.to_string()]);
    let leading: String = mixed.chars().take(HEX_DIGITS_OF_32_BITS).collect();
    let whole = u32::from_str_radix(&leading, 16).unwrap_or_default();
    i32::try_from(whole & 0x7fff_ffff).unwrap_or_default()
}

/// One value as GraphSON 3.0 writes it.
///
/// A string and a boolean are written as themselves; every number carries the type it is
/// read as, a list and a map carry theirs, and a map's entries are written as a flat
/// sequence of key and value, which is the form GraphSON writes a map in.
fn graphson_value(value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(flag) => Value::Bool(*flag),
        Value::String(text) => Value::String(text.clone()),
        Value::Number(number) => match (number.as_i64(), number.as_u64(), number.as_f64()) {
            (Some(whole), _, _) => typed_integer(whole),
            (None, Some(whole), _) => ::serde_json::json!({
                GRAPHSON_TYPE: GRAPHSON_INT64,
                GRAPHSON_VALUE: whole,
            }),
            (None, None, Some(fraction)) => ::serde_json::json!({
                GRAPHSON_TYPE: GRAPHSON_DOUBLE,
                GRAPHSON_VALUE: fraction,
            }),
            (None, None, None) => Value::Null,
        },
        Value::Array(items) => ::serde_json::json!({
            GRAPHSON_TYPE: GRAPHSON_LIST,
            GRAPHSON_VALUE: items.iter().map(graphson_value).collect::<Vec<Value>>(),
        }),
        Value::Object(entries) => {
            let mut flat = Vec::with_capacity(entries.len() * 2);
            for (key, entry) in entries {
                flat.push(Value::String(key.clone()));
                flat.push(graphson_value(entry));
            }
            ::serde_json::json!({
                GRAPHSON_TYPE: GRAPHSON_MAP,
                GRAPHSON_VALUE: flat,
            })
        }
    }
}

/// One whole number as GraphSON writes it: the 32-bit type for a value that is one, and
/// the 64-bit type for one that is not, as the Java types a traversal produces are
/// written.
fn typed_integer(whole: i64) -> Value {
    let name = if i32::try_from(whole).is_ok() {
        GRAPHSON_INT32
    } else {
        GRAPHSON_INT64
    };
    ::serde_json::json!({ GRAPHSON_TYPE: name, GRAPHSON_VALUE: whole })
}

/// The results of a run as the `result` member of the Gremlin endpoint's answer carries
/// them: the data the traversal answered, and the metadata of that answer.
#[must_use]
pub fn result_document(data: &[Value]) -> ::aws_smithy_types::Document {
    let result = ::serde_json::json!({
        "data": {
            GRAPHSON_TYPE: GRAPHSON_LIST,
            GRAPHSON_VALUE: data,
        },
        "meta": empty_map(),
    });
    super::json_to_document(&result)
}

/// The attributes a served answer's status carries: none, as the empty typed map.
#[must_use]
pub fn status_attributes() -> ::aws_smithy_types::Document {
    super::json_to_document(&empty_map())
}

/// The empty GraphSON map, which is what an answer carrying no metadata and no status
/// attribute reports.
fn empty_map() -> Value {
    ::serde_json::json!({ GRAPHSON_TYPE: GRAPHSON_MAP, GRAPHSON_VALUE: [] })
}

/// One value a `has` compares a property against.
#[derive(Debug, Clone)]
enum Predicate {
    Equal(Value),
    NotEqual(Value),
    Less(Value),
    AtMost(Value),
    Greater(Value),
    AtLeast(Value),
    /// Both bounds of a range: `between` includes the lower one and excludes the upper
    /// one, as the predicate documents them.
    Between(Value, Value),
    Inside(Value, Value),
    Outside(Value, Value),
    Within(Vec<Value>),
    Without(Vec<Value>),
    Containing(String),
    NotContaining(String),
    StartingWith(String),
    NotStartingWith(String),
    EndingWith(String),
    NotEndingWith(String),
}

impl Predicate {
    /// Whether the property value `held` satisfies this predicate.
    ///
    /// A property holding several values satisfies one when any of its values does, as a
    /// multi-valued property is matched.
    fn admits(&self, held: &Value) -> bool {
        if let Value::Array(values) = held {
            return values.iter().any(|value| self.admits(value));
        }
        match self {
            Self::Equal(value) => held == value,
            Self::NotEqual(value) => held != value,
            Self::Less(value) => compared(held, value).is_some_and(|order| order.is_lt()),
            Self::AtMost(value) => compared(held, value).is_some_and(|order| order.is_le()),
            Self::Greater(value) => compared(held, value).is_some_and(|order| order.is_gt()),
            Self::AtLeast(value) => compared(held, value).is_some_and(|order| order.is_ge()),
            // `between(low, high)` is "at least low and less than high", as the predicate
            // documents its bounds.
            Self::Between(low, high) => {
                compared(held, low).is_some_and(|order| order.is_ge())
                    && compared(held, high).is_some_and(|order| order.is_lt())
            }
            Self::Inside(low, high) => {
                compared(held, low).is_some_and(|order| order.is_gt())
                    && compared(held, high).is_some_and(|order| order.is_lt())
            }
            Self::Outside(low, high) => {
                compared(held, low).is_some_and(|order| order.is_lt())
                    || compared(held, high).is_some_and(|order| order.is_gt())
            }
            Self::Within(values) => values.contains(held),
            Self::Without(values) => !values.contains(held),
            Self::Containing(text) => text_of(held).is_some_and(|held| held.contains(text)),
            Self::NotContaining(text) => text_of(held).is_some_and(|held| !held.contains(text)),
            Self::StartingWith(text) => text_of(held).is_some_and(|held| held.starts_with(text)),
            Self::NotStartingWith(text) => {
                text_of(held).is_some_and(|held| !held.starts_with(text))
            }
            Self::EndingWith(text) => text_of(held).is_some_and(|held| held.ends_with(text)),
            Self::NotEndingWith(text) => text_of(held).is_some_and(|held| !held.ends_with(text)),
        }
    }
}

/// `held` as text, for a predicate that compares text.
fn text_of(held: &Value) -> Option<&str> {
    held.as_str()
}

/// How `held` orders against `value`, or `None` for two values that are not of one kind —
/// which no ordering predicate admits, because there is no order between them.
fn compared(held: &Value, value: &Value) -> Option<::std::cmp::Ordering> {
    match (held, value) {
        (Value::Number(held), Value::Number(value)) => {
            held.as_f64()?.partial_cmp(&value.as_f64()?)
        }
        (Value::String(held), Value::String(value)) => Some(held.cmp(value)),
        (Value::Bool(held), Value::Bool(value)) => Some(held.cmp(value)),
        _ => None,
    }
}

/// The value an argument IS: a literal, never an expression.
fn literal(argument: &Argument) -> Result<Value, TraversalFailure> {
    match argument {
        Argument::Text(text) => Ok(Value::String(text.clone())),
        Argument::Number(number) => number
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| number.parse::<f64>().map(Value::from))
            .map_err(|_| {
                TraversalFailure::Refused(format!(
                    "{} is not a number this engine reads",
                    super::echoed(number)
                ))
            }),
        Argument::Boolean(flag) => Ok(Value::Bool(*flag)),
        Argument::Null => Ok(Value::Null),
        Argument::List(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(literal(item)?);
            }
            Ok(Value::Array(values))
        }
        Argument::Expression(_) => Err(TraversalFailure::NotEvaluated {
            step: "an expression argument".to_string(),
        }),
    }
}

/// The stream with the results it carries more than once removed, keeping the first of
/// each — which is what a step that answers each result once answers.
///
/// Each result is looked up in a SET of the results already kept, keyed by a canonical
/// form of the result, so the step costs one lookup per result rather than one comparison
/// per result per result; and each result is charged against the run's budget, so the work
/// ceiling and the run's deadline stop this step like every other one.
fn deduplicated(budget: &Budget, stream: Vec<Item>) -> Result<Vec<Item>, TraversalFailure> {
    let mut kept: Vec<Item> = Vec::with_capacity(stream.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for item in stream {
        let key = dedup_key(&item);
        budget.charge_size(key.len(), MAX_RESULT_CHARS)?;
        if seen.insert(key) {
            kept.push(item);
        }
    }
    Ok(kept)
}

/// One result as a key a set holds it under: two results have the same key exactly when
/// they are the same result.
fn dedup_key(item: &Item) -> String {
    match item {
        Item::Vertex(id) => format!("v\u{1f}{id}"),
        Item::Edge(id) => format!("e\u{1f}{id}"),
        Item::Count(count) => format!("c\u{1f}{count}"),
        Item::Data(value) => format!("d\u{1f}{value}"),
    }
}

/// The id of the node `item` names, for a step that walks from one.
fn vertex_of(item: &Item) -> Result<String, TraversalFailure> {
    match item {
        Item::Vertex(id) => Ok(id.clone()),
        _ => Err(TraversalFailure::Refused(
            "A step that walks the graph starts from a vertex".to_string(),
        )),
    }
}

/// Refuse a step applied to a result that is not an element of the graph.
///
/// A filter asks something about an ELEMENT, so a traverser carrying a value or a count is
/// not one it can answer about: the traversal is refused rather than answered with the
/// empty result, which a caller cannot tell from a filter that really matched nothing.
fn applied_to_element(item: &Item, step: &str) -> Result<(), TraversalFailure> {
    match item {
        Item::Vertex(_) | Item::Edge(_) => Ok(()),
        Item::Count(_) | Item::Data(_) => Err(not_an_element(step)),
    }
}

/// The refusal for a step applied to something that is not an element of the graph.
fn not_an_element(step: &str) -> TraversalFailure {
    TraversalFailure::Refused(format!(
        "The step {step}() is applied to an element of the graph, and this result is not one"
    ))
}

/// The refusal for a step whose argument has to be an element id and is not.
fn not_an_id(step: &str) -> TraversalFailure {
    TraversalFailure::Refused(format!(
        "The step {step}() takes the ids of the elements it names"
    ))
}

/// The refusal for a step whose argument has to be a name (a label, a type, a property
/// key) and is not.
fn not_a_name(step: &str) -> TraversalFailure {
    TraversalFailure::Refused(format!(
        "The step {step}() takes names, each written as a string"
    ))
}
