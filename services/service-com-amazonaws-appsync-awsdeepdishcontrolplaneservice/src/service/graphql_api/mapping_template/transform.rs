// SPDX-License-Identifier: CC0-1.0

//! The `$util.transform` helpers a mapping template may call — the transformation family of
//! the AppSync resolver mapping-template utility reference, "to simplify the creation of
//! resolvers for the data sources".
//!
//! What the family does is ONE translation, and it is not this module's: a filter stated as an
//! object — one entry per attribute, whose value names the operators that attribute is compared
//! under, with `and`, `or` and `not` grouping entries — comes to the `{expression,
//! expressionNames, expressionValues}` document a DynamoDB request object carries it as, built
//! by the one reading of that language this crate has
//! ([`super::super::dynamodb::helpers::published_filter_expression`]), which a resolver written
//! for the APPSYNC_JS runtime reaches as well. The helper answers that document as JSON TEXT,
//! which is why a template that wants to read a member of it parses the answer first
//! (`$util.parseJson($util.transform.…)`) and one that forwards it whole passes the text
//! straight into the request object it is writing.
//!
//! What IS this module's is the one thing that reading does not decide: how a filter the
//! translation cannot build an expression from is reported. It FAILS the field being resolved
//! rather than the evaluation — the helper raises the error class AppSync classifies a
//! transformation failure under ([`FILTER_ERROR_TYPE`]), stating the input it was handed as the
//! Java collections a resolver runs over write themselves — so a client branching on
//! `errorType` tells a filter failure apart from an authorization denial or a resolver-authored
//! error. The translation's own sentence is not carried: AWS words this failure from the input,
//! not from the clause the walk stopped at.
//!
//! Nothing here reads or writes any state: a helper is a pure transformation of the value the
//! call was made with.

use ::serde_json::Value as Json;

use super::super::dynamodb::helpers::published_filter_expression;
use super::utilities::{Answer, Raised};

/// What one `$util.transform.<helper>` call came to, or `None` when `helper` names no
/// helper of this family — which the registry ([`super::utilities`]) reports as the fault
/// naming it.
pub fn call(helper: &str, args: &[Json]) -> Option<Answer> {
    match helper {
        TO_FILTER_EXPRESSION => {
            let stated = args.first().cloned().unwrap_or(Json::Null);
            Some(match published_filter_expression(&stated) {
                Ok(document) => Answer::Value(Json::String(document.to_string())),
                // The failure is the FIELD's: it carries the class AppSync classifies a
                // transformation failure under and states the input it could not build an
                // expression from, which is what the caller has to change.
                Err(_) => Answer::Raised(Raised {
                    message: format!("{FILTER_FAILED}{}", written(&stated)),
                    error_type: Some(FILTER_ERROR_TYPE.to_string()),
                    ..Raised::default()
                }),
            })
        }
        _ => None,
    }
}

/// `value` written the way the Java collections an AppSync resolver runs over write
/// themselves — a map as `{name=value}`, a list as `[element, element]` and a string as its
/// own text — which is how the failure of a transformation states the input it was handed.
///
/// The walk stops at the depth the translation itself reads a filter to
/// ([`MAX_STATED_DEPTH`]), writing a composite past it as the empty one: the input is
/// caller-supplied, so what it costs the host's stack is bounded here rather than by how
/// deeply the caller nested it.
fn written(value: &Json) -> String {
    written_to_depth(value, 0)
}

/// See [`written`].
fn written_to_depth(value: &Json, depth: usize) -> String {
    match value {
        Json::Null => "null".to_string(),
        Json::Bool(decided) => decided.to_string(),
        Json::Number(count) => count.to_string(),
        Json::String(text) => text.clone(),
        Json::Array(_) | Json::Object(_) if depth > MAX_STATED_DEPTH => match value {
            Json::Array(_) => "[]".to_string(),
            _ => "{}".to_string(),
        },
        Json::Array(elements) => format!(
            "[{}]",
            elements
                .iter()
                .map(|element| written_to_depth(element, depth.saturating_add(1)))
                .collect::<Vec<String>>()
                .join(", ")
        ),
        Json::Object(members) => format!(
            "{{{}}}",
            members
                .iter()
                .map(|(name, member)| format!(
                    "{name}={}",
                    written_to_depth(member, depth.saturating_add(1))
                ))
                .collect::<Vec<String>>()
                .join(", ")
        ),
    }
}

/// How deeply the input a refusal states is walked, which is the depth the translation reads a
/// filter to: a bound of this emulator's own, as every other ceiling an evaluation is held to.
const MAX_STATED_DEPTH: usize = 16;

/// The helper of this family.
const TO_FILTER_EXPRESSION: &str = "toDynamoDBFilterExpression";

/// The fixed text a transformation that could not be made states, and the class AppSync
/// classifies such a failure under — what a client branching on `errorType` reads a filter
/// failure as.
const FILTER_FAILED: &str = "Error generating DynamoDB filter expression from input - ";
/// See [`FILTER_FAILED`].
pub const FILTER_ERROR_TYPE: &str = "DynamoDBFilterError";
