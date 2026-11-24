// SPDX-License-Identifier: CC0-1.0

//! What an openCypher request carries, and the modelled refusal each failure is reported
//! through.
//!
//! Both openCypher endpoints — the one that EVALUATES a query and the one that EXPLAINS
//! it — take the same two members (the query and its parameters), bound the same way, and
//! refuse the same things about them. So the reading of `parameters` and the mapping from
//! a [`QueryFailure`] to the modelled error whose own documentation states that fact live
//! here once: the two operations differ in what they ANSWER with, not in what they refuse.

use std::collections::BTreeMap;

use ::serde_json::Value;

use crate::ops::types::{
    ConstraintViolationException, FailureByQueryException, InvalidParameterException,
    MalformedQueryException, MemoryLimitExceededException, QueryLimitException,
    QueryTooLargeException, TimeLimitExceededException, UnsupportedOperationException,
};

use super::super::property_graph::GraphWriteError;
use super::{QueryFailure, MAX_PARAMETERS_CHARS};

/// The input member an openCypher query arrives in, by its wire name (`openCypherQuery`'s
/// `jsonName`), which is the name a refusal has to name for the caller to fix the request
/// from it.
pub const OPEN_CYPHER_QUERY_MEMBER: &str = "query";

/// The input member the query's parameters arrive in, by its wire name.
pub const PARAMETERS_MEMBER: &str = "parameters";

/// The parameters the request carried, by name.
///
/// `parameters` is modelled as a String, and the member's own documentation names it "the
/// openCypher query parameters", which Neptune takes as a JSON object of parameter name to
/// value. A member carrying anything else cannot bind a parameter, so it is refused with
/// what was wrong rather than read as no parameters at all.
///
/// The text is bounded before it is read ([`MAX_PARAMETERS_CHARS`]): every value a
/// parameter binds comes out of it, so bounding the text bounds what one request can hand
/// the evaluation.
pub fn request_parameters(
    supplied: Option<&str>,
) -> Result<BTreeMap<String, Value>, String> {
    let Some(text) = supplied else {
        return Ok(BTreeMap::new());
    };
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let characters = text.chars().count();
    if characters > MAX_PARAMETERS_CHARS {
        return Err(format!(
            "Supplied {PARAMETERS_MEMBER} is {characters} characters, and this engine reads \
             at most {MAX_PARAMETERS_CHARS}"
        ));
    }
    let parsed: Value = ::serde_json::from_str(text)
        .map_err(|refused| format!("Supplied {PARAMETERS_MEMBER} is not valid JSON: {refused}"))?;
    match parsed {
        Value::Object(members) => Ok(members.into_iter().collect()),
        _ => Err(format!(
            "Supplied {PARAMETERS_MEMBER} must be a JSON object of parameter names to values"
        )),
    }
}

/// How one failure is recorded in the access log: the KIND of refusal, never the query or
/// the values it carried.
#[must_use]
pub fn query_outcome(failure: &QueryFailure) -> &'static str {
    match failure {
        QueryFailure::Malformed { .. } => "malformed-query",
        QueryFailure::Unsupported { .. } => "unsupported-clause",
        QueryFailure::Refused(_) => "refused-query",
        QueryFailure::Constraint(_) => "constraint-violation",
        QueryFailure::MissingParameter(_) => "missing-parameter",
        QueryFailure::TooLong { .. } => "query-too-large",
        QueryFailure::TooDeep { .. }
        | QueryFailure::TooLarge { .. }
        | QueryFailure::ValueTooLarge { .. }
        | QueryFailure::TooNested { .. }
        | QueryFailure::TooManyHops { .. }
        | QueryFailure::Exhausted { .. } => "query-limit",
        QueryFailure::TimedOut { .. } => "time-limit",
        QueryFailure::Write(_) => "write-refused",
        QueryFailure::Interrupted(_) => "engine-failure",
    }
}

/// The modelled error one failure is reported through, with its payload.
///
/// One variant per modelled error the openCypher endpoints refuse with. Each operation
/// lifts these into its OWN generated error enum, which is why the payload is carried
/// here rather than the variant of one operation's enum.
#[derive(Debug)]
pub enum QueryRefusal {
    /// The text could not be read as a query, or could be read and not evaluated.
    Malformed(MalformedQueryException),
    /// The query names a clause this engine does not run.
    Unsupported(UnsupportedOperationException),
    /// The query asked for something the graph's own rules do not allow.
    Constraint(ConstraintViolationException),
    /// The `parameters` member could not bind what the query references.
    InvalidParameter(InvalidParameterException),
    /// The query text is longer than this engine reads.
    TooLarge(QueryTooLargeException),
    /// The query asked for more than a system limit allows.
    Limit(QueryLimitException),
    /// The span the run was given ran out while it was still working.
    TimeLimit(TimeLimitExceededException),
    /// The store had no room for a write the query asked for.
    Memory(MemoryLimitExceededException),
    /// The run could not be carried out at all — a server-side failure.
    Failure(FailureByQueryException),
}

/// The modelled error `failure` is reported through.
///
/// Each variant of [`QueryFailure`] is a different fact about the request, so each is
/// reported through the error whose own documentation states that fact — the query could
/// not be read, names a clause this engine does not run, asked for more than a system
/// limit allows, ran out of time, or could not be served at all.
#[must_use]
pub fn query_refusal(failure: QueryFailure) -> QueryRefusal {
    match failure {
        QueryFailure::Malformed { reason } => {
            QueryRefusal::Malformed(super::malformed_open_cypher_query(&reason))
        }
        // A well-formed query this engine does not run is a different answer to the
        // caller than a text that is not a query.
        QueryFailure::Unsupported { clause } => {
            QueryRefusal::Unsupported(super::open_cypher_unsupported(clause))
        }
        QueryFailure::Refused(reason) => {
            QueryRefusal::Malformed(super::malformed_open_cypher_query(&reason))
        }
        QueryFailure::Constraint(reason) => {
            QueryRefusal::Constraint(super::open_cypher_constraint(&reason))
        }
        QueryFailure::MissingParameter(name) => QueryRefusal::InvalidParameter(
            super::invalid_open_cypher_parameters(&format!(
                "Supplied {PARAMETERS_MEMBER} does not carry the parameter '{name}' \
                 the query references"
            )),
        ),
        QueryFailure::TooLong {
            characters,
            ceiling,
        } => QueryRefusal::TooLarge(super::open_cypher_query_too_large(&format!(
            "the query is {characters} characters, and this engine reads at most {ceiling}"
        ))),
        QueryFailure::TooDeep { depth, ceiling } => {
            QueryRefusal::Limit(super::open_cypher_query_limit(&format!(
                "the query nests {depth} deep, and this engine reads at most {ceiling}"
            )))
        }
        QueryFailure::TooLarge { rows, ceiling } => {
            QueryRefusal::Limit(super::open_cypher_query_limit(&format!(
                "the query answers {rows} rows, and this engine answers at most {ceiling}"
            )))
        }
        QueryFailure::ValueTooLarge { size, ceiling } => {
            QueryRefusal::Limit(super::open_cypher_query_limit(&format!(
                "the query builds a value of {size}, and this engine builds at most {ceiling}"
            )))
        }
        QueryFailure::TooNested { ceiling } => {
            QueryRefusal::Limit(super::open_cypher_query_limit(&format!(
                "the query evaluates deeper than the {ceiling} levels one query may descend"
            )))
        }
        QueryFailure::TooManyHops { asked, ceiling } => {
            QueryRefusal::Limit(super::open_cypher_query_limit(&format!(
                "the query walks up to {asked} hops, and this engine walks at most {ceiling}"
            )))
        }
        QueryFailure::Exhausted { ceiling } => {
            QueryRefusal::Limit(super::open_cypher_query_limit(&format!(
                "the query asked for more than the {ceiling} units of work one query may spend"
            )))
        }
        QueryFailure::TimedOut { ceiling } => {
            QueryRefusal::TimeLimit(super::open_cypher_time_limit(&format!(
                "one query runs for at most {} seconds",
                ceiling.as_secs()
            )))
        }
        QueryFailure::Write(refused) => write_refusal(refused),
        QueryFailure::Interrupted(reason) => {
            QueryRefusal::Failure(super::engine_failure(&reason))
        }
    }
}

/// The modelled error a write the store could not take is reported through: a store with
/// no room left for the element is a resource limit, and a write the graph's own rules
/// refuse is a constraint the request did not satisfy.
fn write_refusal(refused: GraphWriteError) -> QueryRefusal {
    let (ceiling, reason) = write_refusal_reason(&refused);
    match ceiling {
        WriteCeiling::Room => QueryRefusal::Memory(super::open_cypher_memory_limit(&reason)),
        WriteCeiling::Rule => QueryRefusal::Constraint(super::open_cypher_constraint(&reason)),
    }
}

/// WHICH kind of ceiling a write the store refused met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCeiling {
    /// The store had no room left for the element: a resource limit of this engine.
    Room,
    /// The write asked for something the graph's own rules do not allow.
    Rule,
}

/// The kind of ceiling a write the store refused met, and the sentence stating what it
/// met.
///
/// The sentence is about the GRAPH rather than about either query language, so it is
/// stated once here and each language's endpoints wrap it in their own modelled payload
/// ([`write_refusal`] for openCypher, `super::super::gremlin_eval` for Gremlin).
#[must_use]
pub fn write_refusal_reason(refused: &GraphWriteError) -> (WriteCeiling, String) {
    match refused {
        GraphWriteError::NodeLimit { ceiling } => (
            WriteCeiling::Room,
            format!("this engine holds at most {ceiling} nodes"),
        ),
        GraphWriteError::RelationshipLimit { ceiling } => (
            WriteCeiling::Room,
            format!("this engine holds at most {ceiling} relationships"),
        ),
        GraphWriteError::ByteLimit { ceiling } => (
            WriteCeiling::Room,
            format!("this engine holds at most {ceiling} bytes of element data"),
        ),
        GraphWriteError::LabelLimit { ceiling } => (
            WriteCeiling::Rule,
            format!("A node carries at most {ceiling} labels"),
        ),
        GraphWriteError::NameTooLong { size, ceiling } => (
            WriteCeiling::Rule,
            format!(
                "An id, a label and a relationship type are at most {ceiling} bytes, \
                 and this one is {size}"
            ),
        ),
        GraphWriteError::DuplicateId { id } => (
            WriteCeiling::Rule,
            format!(
                "An element with the id {} already exists",
                super::super::echoed(id)
            ),
        ),
        GraphWriteError::MissingEndpoint { id } => (
            WriteCeiling::Rule,
            format!(
                "A relationship joins two nodes this graph holds, and {} is not one",
                super::super::echoed(id)
            ),
        ),
    }
}
