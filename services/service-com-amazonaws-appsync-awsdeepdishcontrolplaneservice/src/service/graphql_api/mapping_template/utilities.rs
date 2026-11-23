// SPDX-License-Identifier: CC0-1.0

//! The `$util` helpers a mapping template may call — "general utility methods … available
//! to all templates" in the AppSync resolver mapping-template reference.
//!
//! One function per helper, over the same JSON value model the rest of the language works
//! in ([`super::values`]). The `$util.dynamodb` family — the helpers that MARSHAL a value as
//! the attribute value DynamoDB holds it as — lives in [`dynamodb`], one module for one
//! family. Three of them are not values at all and are answered as their own
//! outcome: `$util.quiet` produces nothing, and `$util.error` and `$util.unauthorized` RAISE
//! — "throws a custom error", which is how a template refuses the field it is resolving.
//!
//! A helper this does not implement is a FAULT, named in the answer, rather than an empty
//! value: a template asking for one gets the field reported as unresolved instead of a value
//! nothing computed.

use ::serde_json::{Map as JsonMap, Value as Json};

use super::values;
// The plane's ONE reading of the refusal `$util.unauthorized` raises, so a template and the
// executor cannot word it differently.
use crate::service::graphql_api::field_auth::{
    unauthorized_field_message, UNAUTHORIZED_FIELD_ERROR_TYPE,
};

pub mod dynamodb;

/// What one `$util` call came to.
#[derive(Debug, Clone)]
pub enum Answer {
    /// A value, written where the call was referenced.
    Value(Json),
    /// A call that produces no output at all: `$util.quiet` / `$util.qr`, "the same as
    /// `$util.quiet`".
    Nothing,
    /// A call that RAISES: the field being resolved did not resolve, and this is the error
    /// it reports.
    Raised(Raised),
    /// A call that APPENDS an error and goes on: `$util.appendError`, which "appends a
    /// custom error … does not interrupt the evaluation of the request mapping template".
    Appended(Raised),
}

/// The error a template raises or appends, with every member the helper takes.
#[derive(Debug, Clone, Default)]
pub struct Raised {
    pub message: String,
    pub error_type: Option<String>,
    pub data: Option<Json>,
    pub error_info: Option<Json>,
    /// Set only by `$util.unauthorized` — "throws Unauthorized for the field being resolved"
    /// — which REFUSES the field rather than reporting an error the author worded: the caller
    /// that resolves a field words that refusal itself, because it alone knows the field and
    /// the type it is of. An error of the author's own carries `false`, whatever class it
    /// names, so `$util.error("…", "Unauthorized")` keeps its own sentence.
    pub refuses_the_field: bool,
}

impl Raised {
    /// The error as one entry of an `outErrors` list: every member the helper takes, each
    /// one the raise named none of carried as `null`.
    ///
    /// This is the projection a SANDBOX evaluation reports an appended error by
    /// (`crate::ops::evaluate_mapping_template`), the same one the APPSYNC_JS runtime of the
    /// same sandbox writes (`crate::ops::evaluate_code::appsync_js`), so one appended error
    /// reads the same whichever language appended it. The entry of a GraphQL result
    /// ([`Self::document`]) is the other projection: it names the field's path and leaves
    /// out what the raise did not name.
    #[must_use]
    pub fn detail(&self) -> Json {
        let member = |value: Option<Json>| value.unwrap_or(Json::Null);
        let mut entry = JsonMap::new();
        entry.insert("message".to_string(), Json::String(self.message.clone()));
        entry.insert(
            "errorType".to_string(),
            member(self.error_type.clone().map(Json::String)),
        );
        entry.insert("data".to_string(), member(self.data.clone()));
        entry.insert("errorInfo".to_string(), member(self.error_info.clone()));
        Json::Object(entry)
    }

    /// One such entry read back as the error it describes — the inverse of [`Self::detail`].
    ///
    /// What appends an error inside the SANDBOX reports its entries in exactly that
    /// projection, as JSON text (`crate::ops::evaluate_code::appsync_js`), so this is how an
    /// error a definition written in that runtime appended reaches the execution that reports
    /// it beside the field's value. A member the entry carries as `null` was named by no
    /// raise, so it is left unset; an appended error never REFUSES the field — only
    /// `$util.unauthorized` does, and that one raises.
    #[must_use]
    pub fn of_detail(entry: &Json) -> Self {
        let named = |member: &str| {
            entry
                .get(member)
                .filter(|value| !value.is_null())
                .cloned()
        };
        Self {
            message: entry
                .get("message")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            error_type: named("errorType")
                .and_then(|value| value.as_str().map(str::to_string)),
            data: named("data"),
            error_info: named("errorInfo"),
            refuses_the_field: false,
        }
    }

    /// The error as one entry of a GraphQL result's `errors`.
    #[must_use]
    pub fn document(&self, path: &[Json]) -> Json {
        let mut entry = JsonMap::new();
        entry.insert("path".to_string(), Json::Array(path.to_vec()));
        entry.insert("message".to_string(), Json::String(self.message.clone()));
        if let Some(error_type) = &self.error_type {
            entry.insert("errorType".to_string(), Json::String(error_type.clone()));
        }
        if let Some(data) = &self.data {
            entry.insert("data".to_string(), data.clone());
        }
        if let Some(error_info) = &self.error_info {
            entry.insert("errorInfo".to_string(), error_info.clone());
        }
        Json::Object(entry)
    }
}

/// Call the `$util` helper `path` names with `args`.
///
/// `path` is the helper's own name, or the name of a sub-helper and the name within it
/// (`$util.time.nowISO8601` arrives as `["time", "nowISO8601"]`). `context` is the
/// `$context` the template is being evaluated over, which is what the one helper that
/// refuses the FIELD being resolved names it from.
pub fn call(path: &[String], args: &[Json], context: &Json) -> Result<Answer, String> {
    let named: Vec<&str> = path.iter().map(String::as_str).collect();
    let value = |produced: Json| Ok(Answer::Value(produced));
    let argument = |index: usize| args.get(index).cloned().unwrap_or(Json::Null);
    let text = |index: usize| match args.get(index) {
        Some(Json::String(text)) => text.clone(),
        Some(other) => values::rendered(other),
        None => String::new(),
    };
    match named.as_slice() {
        // "Returns a stringified JSON representation of the object".
        ["toJson"] => value(Json::String(argument(0).to_string())),
        // "Takes a stringified JSON and returns an object representation of the result".
        ["parseJson"] => match ::serde_json::from_str::<Json>(&text(0)) {
            Ok(parsed) => value(parsed),
            Err(_) => Err(NOT_JSON.to_string()),
        },
        // "Returns an empty string" — the call is made for its effect, not its value.
        ["quiet" | "qr"] => Ok(Answer::Nothing),
        // "Throws a custom error": message, then the optional errorType, data and
        // errorInfo, in the order the helper takes them.
        ["error"] => Ok(Answer::Raised(raised(args))),
        // "Appends a custom error", which does not stop the evaluation.
        ["appendError"] => Ok(Answer::Appended(raised(args))),
        // "Throws Unauthorized for the field being resolved."
        ["unauthorized"] => {
            let named = |member: &str| {
                context
                    .get("info")
                    .and_then(|info| info.get(member))
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            Ok(Answer::Raised(Raised {
                message: unauthorized_field_message(
                    &named("parentTypeName"),
                    &named("fieldName"),
                ),
                error_type: Some(UNAUTHORIZED_FIELD_ERROR_TYPE.to_string()),
                refuses_the_field: true,
                ..Raised::default()
            }))
        }
        // "Returns a 128-bit randomly generated UUID."
        ["autoId"] => value(Json::String(::uuid::Uuid::new_v4().to_string())),
        // The null and emptiness tests.
        ["isNull"] => value(Json::Bool(argument(0).is_null())),
        ["isNotNull"] => value(Json::Bool(!argument(0).is_null())),
        ["isNullOrEmpty"] => value(Json::Bool(is_empty(&argument(0)))),
        ["isNullOrBlank"] => value(Json::Bool(match argument(0) {
            Json::String(text) => text.trim().is_empty(),
            other => is_empty(&other),
        })),
        ["defaultIfNull"] => value(match argument(0) {
            Json::Null => argument(1),
            present => present,
        }),
        ["defaultIfNullOrEmpty" | "defaultIfNullOrBlank"] => {
            let candidate = argument(0);
            let blank = match &candidate {
                Json::String(text) => text.trim().is_empty(),
                other => is_empty(other),
            };
            value(if blank { argument(1) } else { candidate })
        }
        // The type tests.
        ["isString"] => value(Json::Bool(argument(0).is_string())),
        ["isNumber"] => value(Json::Bool(argument(0).is_number())),
        ["isBoolean"] => value(Json::Bool(argument(0).is_boolean())),
        ["isList"] => value(Json::Bool(argument(0).is_array())),
        ["isMap"] => value(Json::Bool(argument(0).is_object())),
        // The encodings.
        ["escapeJavaScript"] => value(Json::String(escaped(&text(0)))),
        ["urlEncode"] => value(Json::String(
            ::url::form_urlencoded::byte_serialize(text(0).as_bytes()).collect(),
        )),
        ["urlDecode"] => value(Json::String(
            ::url::form_urlencoded::parse(format!("value={}", text(0)).as_bytes())
                .find(|(key, _)| key == "value")
                .map(|(_, decoded)| decoded.to_string())
                .unwrap_or_default(),
        )),
        ["base64Encode"] => {
            use ::base64::Engine as _;
            value(Json::String(
                ::base64::engine::general_purpose::STANDARD.encode(text(0).as_bytes()),
            ))
        }
        ["base64Decode"] => {
            use ::base64::Engine as _;
            match ::base64::engine::general_purpose::STANDARD.decode(text(0)) {
                Ok(decoded) => value(Json::String(
                    String::from_utf8(decoded).map_err(|_| NOT_TEXT.to_string())?,
                )),
                Err(_) => Err(NOT_BASE64.to_string()),
            }
        }
        // "Returns true if the specified pattern in the first argument matches the supplied
        // data in the second argument." The pattern comes from the TEMPLATE (or from the
        // request, through a reference), so it is compiled under an explicit program budget
        // rather than the engine's own defaults.
        ["matches"] => {
            let compiled = ::regex::RegexBuilder::new(&text(0))
                .size_limit(MAX_PATTERN_PROGRAM_BYTES)
                .dfa_size_limit(MAX_PATTERN_DFA_BYTES)
                .build();
            match compiled {
                Ok(expression) => value(Json::Bool(expression.is_match(&text(1)))),
                Err(_) => Err(NOT_A_PATTERN.to_string()),
            }
        }
        // The DynamoDB marshalling helpers: each renders a value as the attribute value
        // DynamoDB holds it as, and each has a sibling that answers the same representation
        // serialized ([`dynamodb`]).
        ["dynamodb", helper] => match dynamodb::call(helper, args)? {
            Some(produced) => value(produced),
            None => Err(format!("{NO_SUCH_HELPER}$util.dynamodb.{helper}")),
        },
        // The relational-database helpers: the statement builders a request template writes
        // its statements with, and the reading of the result one produced
        // ([`super::super::relational_database::utilities`]), which is the same reading an
        // APPSYNC_JS resolver's `@aws-appsync/utils/rds` import reaches.
        ["rds", helper] => {
            match super::super::relational_database::utilities::call(helper, args)? {
                Some(produced) => value(produced),
                None => Err(format!("{NO_SUCH_HELPER}$util.rds.{helper}")),
            }
        }
        // The clock helpers, which read the one clock this process has.
        ["time", "nowISO8601"] => value(Json::String(now_iso8601())),
        ["time", "nowEpochSeconds"] => value(values::number(unix_millis() as f64 / 1000.0)),
        ["time", "nowEpochMilliSeconds"] => value(values::number(unix_millis() as f64)),
        unknown => Err(format!("{NO_SUCH_HELPER}$util.{}", unknown.join("."))),
    }
}

/// The error the `$util.error` / `$util.appendError` arguments describe, in the order the
/// helpers take them: the message, then the `errorType`, the `data` and the `errorInfo`.
fn raised(args: &[Json]) -> Raised {
    let text = |index: usize| match args.get(index) {
        Some(Json::String(text)) => Some(text.clone()),
        Some(Json::Null) | None => None,
        Some(other) => Some(values::rendered(other)),
    };
    Raised {
        message: text(0).unwrap_or_default(),
        error_type: text(1),
        data: args.get(2).cloned().filter(|data| !data.is_null()),
        error_info: args.get(3).cloned().filter(|info| !info.is_null()),
        // An error the TEMPLATE worded, whatever class it names: only
        // `$util.unauthorized`'s own refusal of the field is that.
        refuses_the_field: false,
    }
}

/// Whether a value is null or carries nothing: an empty string, list or map.
fn is_empty(value: &Json) -> bool {
    match value {
        Json::Null => true,
        Json::String(text) => text.is_empty(),
        Json::Array(list) => list.is_empty(),
        Json::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// `subject` with every character a JavaScript string literal cannot carry escaped, which
/// is what the JSON renderer of a string does — the same escapes, over the same alphabet.
fn escaped(subject: &str) -> String {
    let quoted = Json::String(subject.to_string()).to_string();
    quoted
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .map_or(quoted.clone(), ToString::to_string)
}

/// Now, in the format `$util.time.nowISO8601` answers: an RFC 3339 instant in UTC.
fn now_iso8601() -> String {
    ::aws_smithy_types::DateTime::from(::std::time::SystemTime::now())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// Now, in milliseconds since the epoch.
fn unix_millis() -> u64 {
    ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis().min(u128::from(u64::MAX)) as u64)
}

/// How much room a pattern this crate compiles from CALLER TEXT may take: an explicit budget,
/// because the expression comes from a template or from a stored configuration rather than
/// from this crate — a pattern past it is refused like one that does not parse. Well under the
/// crate's own defaults.
///
/// Shared by every reader of a caller-supplied expression — `$util.matches` here and the
/// `identityValidationExpression` an API's Lambda authorizer is gated by
/// ([`crate::service::graphql_api::authorizer`]) — so one crate weighs one pattern one way.
pub const MAX_PATTERN_PROGRAM_BYTES: usize = 64 * 1024;
/// See [`MAX_PATTERN_PROGRAM_BYTES`].
pub const MAX_PATTERN_DFA_BYTES: usize = 256 * 1024;

/// The sentences a helper fault states.
const NOT_JSON: &str = "This value is not a JSON document.";
const NOT_TEXT: &str = "This value does not decode to text.";
const NOT_BASE64: &str = "This value is not base64.";
const NOT_A_PATTERN: &str = "This value is not a regular expression.";
const NO_SUCH_HELPER: &str = "No such helper: ";
