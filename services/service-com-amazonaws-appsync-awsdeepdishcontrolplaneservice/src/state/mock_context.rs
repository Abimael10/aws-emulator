// SPDX-License-Identifier: CC0-1.0

//! The MOCK CONTEXT a sandbox evaluation is run over, checked against the shape of a resolver
//! context.
//!
//! Both halves of the sandbox take one — `EvaluateMappingTemplate$context` and
//! `EvaluateCode$context`, each "the map that holds all of the contextual information for
//! your resolver invocation. A context is required for this action." — and a caller writes
//! that map by hand to try a template or a definition out. AppSync CHECKS the map it was
//! handed against the shape of a resolver context before anything is evaluated: a property it
//! carries that is not of the kind a resolver context gives that property is reported as one
//! that failed validation, named by its own dotted path, and nothing is evaluated for it.
//!
//! What a resolver context gives each of its properties is what [`MAP_MEMBERS`] states: the
//! arguments map, the parent's resolved value, who the request presented itself as, the
//! request's own fields, where the field sits, the stash, the previous step's result and the
//! error of a step whose data source did not answer are all MAPS. `result` is deliberately
//! not among them — it is whatever the data source produced, so no kind can be required of it
//! — and a property the shape does not name is one no resolver context gives a kind, so it is
//! left to the evaluation to read or to ignore.
//!
//! Nothing here is stored: a mock context is a value of the request it arrived on, and
//! checking one changes nothing a later caller sees.

use ::serde_json::Value as Json;

use crate::ops::evaluate_code::appsync_js::{
    ARGUMENTS_MEMBER, ARGUMENTS_SHORT_MEMBER, STASH_MEMBER,
};

/// The properties of a resolver context that are MAPS, under the names a context document
/// carries them: the same set the data plane builds one of for a resolver step
/// (`crate::service::graphql_api::execution`), which is where that shape is stated for a
/// resolution that is not a mock.
const MAP_MEMBERS: [&str; 8] = [
    ARGUMENTS_MEMBER,
    ARGUMENTS_SHORT_MEMBER,
    "source",
    "identity",
    "request",
    "info",
    STASH_MEMBER,
    "prev",
];

/// The properties of `context` that are NOT of the kind a resolver context gives them, each
/// as the dotted path a caller addresses it by, in the order the shape names them — or `None`
/// where every property it carries is.
///
/// A property the shape does not name is not reported: this is the check AppSync makes of the
/// properties a resolver context HAS, not a rejection of a map that carries more than one.
#[must_use]
pub fn failed_validation(context: &Json) -> Option<Vec<String>> {
    let members = context.as_object()?;
    let failed: Vec<String> = MAP_MEMBERS
        .iter()
        .filter(|name| members.get(**name).is_some_and(|value| !is_map(value)))
        .map(|name| format!("{CONTEXT_PATH}.{name}"))
        .collect();
    (!failed.is_empty()).then_some(failed)
}

/// Whether `value` is a map, as a property a resolver context holds one at.
///
/// A property carrying `null` states nothing about itself — a resolver context's own `source`,
/// `prev` and `error` are null wherever there is no parent, no previous step and no failure —
/// so a null property is not one that failed.
fn is_map(value: &Json) -> bool {
    value.is_object() || value.is_null()
}

/// How the properties of a mock context that failed validation are REPORTED: the fixed
/// sentence, then the dotted path of each offending property, then the period that ends it.
///
/// A SERVED answer rather than a refusal — the sandbox's own `error` member is what carries it
/// — because the call itself was answered: what the caller got wrong is the context it mocked
/// up, and the answer tells it which properties of that mock AppSync could not take.
#[must_use]
pub fn failed_validation_message(failed: &[String]) -> String {
    format!("{FAILED_VALIDATION}{}.", failed.join(", "))
}

/// The root every reported path is written from: the name a mock context's own properties are
/// addressed under.
const CONTEXT_PATH: &str = "ctx";

/// The fixed opening of the sentence a mock context's failed properties are reported with.
const FAILED_VALIDATION: &str =
    "The following properties in your mock context object failed validation: ";
