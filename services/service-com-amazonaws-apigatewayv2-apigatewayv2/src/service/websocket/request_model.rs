// SPDX-License-Identifier: CC0-1.0

//! The route's own REQUEST MODEL: the schema a message served by a WEBSOCKET route has to
//! satisfy, and the refusal a body that does not satisfy it earns.
//!
//! `Route$RequestModels` is "the request models for the route. Supported only for WebSocket
//! APIs", a map whose values name `Model` resources of the same api, and `Model$Schema` is
//! "the schema for the model. For application/json models, this should be JSON schema draft 4
//! model" — so a route that names a model declares what a message it serves must look like,
//! and a message that does not satisfy it is the CLIENT's error rather than something to hand
//! to the route's backend. API Gateway answers such a frame with its own
//! [`INVALID_BODY_MESSAGE`] frame, and the route's integration is not performed at all.
//!
//! Four things decide whether a frame is checked here, in this order:
//!
//! 1. WHICH model the route selects. `Route$ModelSelectionExpression` is "the model selection
//!    expression for the route. Supported only for WebSocket APIs": where it names a path
//!    into the message body it is evaluated against that body exactly as the api's
//!    `RouteSelectionExpression` is (`crate::state::selected_body_value`, this service's one
//!    reader of such a reference), and where it is written as a plain key it IS the key. The
//!    key selects the `RequestModels` entry; a route whose map holds exactly one entry and
//!    nothing that selects another is checked against that one, which is the `$default` entry
//!    the console writes;
//! 2. whether that model is one this stage HOLDS: the name is looked up in the models the
//!    stage's own deployment froze, so a stage pinned to an older deployment checks against
//!    the schema as it stood then;
//! 3. whether the schema is SHORT ENOUGH to be checked against at all
//!    ([`MAX_SCHEMA_BYTES`]);
//! 4. whether the schema is one this emulator can VALIDATE against. A schema that is no JSON
//!    Schema this validator compiles — including one whose `$ref` names a resource outside
//!    the document — states nothing about the message: the frame is served exactly as it
//!    would be for a route that declares no model, rather than refused for a configuration
//!    this emulator could not apply.
//!
//! # What one frame costs, and why it cannot grow
//!
//! Checking a frame PARSES the model's schema, COMPILES it (a `pattern` keyword in it becomes
//! a regex) and evaluates the frame against it, which is work proportional to both documents.
//! Each is bounded by an explicit constant before any of it is done: the schema by
//! [`MAX_SCHEMA_BYTES`], and the frame by the per-message ceiling the socket carrying it
//! applies (`super::transport`), which refuses an oversized message before a route is
//! selected for it. So the cost of one check has a ceiling that no request can raise, however
//! many frames it sends and whatever a caller stored.
//!
//! It is also kept OFF the async worker ([`PendingCheck::rejected`]): the work is synchronous
//! and CPU-bound, so it runs on the blocking pool with a deadline of its own, leaving the task
//! that serves the connection free to be cancelled, timed out and shut down while a check is
//! in flight. The validator's regexes are the `regex` crate's, which has no backtracking, so a
//! stored `pattern` cannot make the evaluation super-linear in the frame either.

use std::time::Duration;

use ::serde_json::Value;

use crate::state::{DeployedConfiguration, Route};

/// The status the stage LOGS for a frame its route's model refused — the client's own error
/// (RFC 9110 §15.5.1) — and the sentence API Gateway's frame carries for it.
pub const INVALID_BODY_STATUS: u16 = 400;
pub const INVALID_BODY_MESSAGE: &str = "Invalid request body";

/// The longest stored schema a frame is checked against: 32768 bytes, the length the member
/// carrying it declares.
///
/// `CreateModel$Schema` and `UpdateModel$Schema` target `StringWithLengthBetween0And32K`,
/// documented as "A string with a length between [0-32768]"
/// (`smithy_query(query="shape", shape="StringWithLengthBetween0And32K")`), so no model AWS
/// stores is longer than this. It is applied HERE, where the schema is about to be parsed and
/// compiled for one frame, so the work one message costs has a ceiling whatever a caller
/// managed to store: a longer document is a configuration this check cannot be applied for,
/// which validates nothing rather than refusing a frame.
pub const MAX_SCHEMA_BYTES: usize = 32 * 1024;

/// How long one check may take on the blocking pool before the frame is served unchecked.
///
/// The work is bounded by [`MAX_SCHEMA_BYTES`] and by the socket's per-message ceiling, so a
/// check that reaches this deadline is not a big input but a pathological one — or a host so
/// loaded that the pool has not got to it yet, which is why it is generous: nothing about the
/// answer depends on how fast the host runs. What it buys is that the task serving the
/// connection is never held by a check that will not finish.
const CHECK_DEADLINE: Duration = Duration::from_secs(30);

/// The `RequestModels` key a route's one entry sits under when nothing selects another, which
/// is the key API Gateway's own console writes — re-exported from the one place this service
/// holds the fallback selection key, so the key a model is selected by and the key an
/// integration response is selected by cannot drift.
use crate::state::integration_response_key::DEFAULT_SELECTION_KEY;

/// What checking one frame against the route's model needs, read out of the store while it
/// was locked and OWNED from then on: the model's name, for the log, and its schema.
///
/// Owned rather than borrowed for the reason the integration beside it is: the check is made
/// with no store lock held, so one connection's frame cannot hold up another's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCheck {
    /// The `Model` the route selected, for this emulator's own log.
    pub model_name: String,
    /// That model's schema, as stored.
    pub schema: String,
}

/// The model check this message owes against `route`, or `None` when the route declares none
/// this stage can apply (see the module docs for the four ways that happens).
pub fn pending(
    deployed: &DeployedConfiguration,
    route: &Route,
    body: &str,
) -> Option<PendingCheck> {
    if route.request_models.is_empty() {
        return None;
    }
    let name = selected_key(route, body)
        .and_then(|key| route.request_models.get(&key))
        .or_else(|| route.request_models.get(DEFAULT_SELECTION_KEY))
        .or_else(|| lone_entry(route))?;
    let stored = deployed
        .models
        .values()
        .find(|model| model.name == *name)
        .map(|model| model.schema.trim())
        .filter(|schema| !schema.is_empty())?;
    // The LENGTH is read before the document is copied out of the store, so a schema past the
    // ceiling costs this check nothing at all — not the copy, and not the parse and compile
    // one frame would otherwise drive ([`MAX_SCHEMA_BYTES`]).
    if stored.len() > MAX_SCHEMA_BYTES {
        ::tracing::warn!(
            model = %name,
            schema_bytes = stored.len(),
            bound_bytes = MAX_SCHEMA_BYTES,
            "this route's model carries a schema longer than a model may be, so a message it \
             serves is checked against nothing"
        );
        return None;
    }
    Some(PendingCheck {
        model_name: name.clone(),
        schema: stored.to_string(),
    })
}

/// The `RequestModels` key this route's `ModelSelectionExpression` selects for `body`, or
/// `None` when the route states no expression at all.
///
/// The expression is EVALUATED (`crate::state::evaluated_selection_expression`, this
/// service's one reading of the grammar): every reference it makes into the message body is
/// replaced by the value that path reaches, so `$request.body.action` selects the value of
/// `action` exactly as the api's `RouteSelectionExpression` does. An expression this call has
/// no value for — a reference naming another namespace, a path the message does not carry, and
/// the plain `$default` a route states to key its one entry — selects the expression's own
/// text instead, which is the key such a route names.
fn selected_key(route: &Route, body: &str) -> Option<String> {
    let expression = route
        .model_selection_expression
        .as_deref()
        .map(str::trim)
        .filter(|expression| !expression.is_empty())?;
    let document: Option<Value> = ::serde_json::from_str(body).ok();
    let selected = crate::state::evaluated_selection_expression(expression, &|reference| {
        let (namespace, path) = crate::state::selection_reference(reference)?;
        if namespace != crate::state::SelectionNamespace::RequestBody {
            return None;
        }
        crate::state::selected_body_value(document.as_ref()?, path)
    });
    Some(selected.unwrap_or_else(|| expression.to_string()))
}

/// The one model name a route's `RequestModels` holds, when it holds exactly one: a map with
/// a single entry is that route's one model whichever key it sits under.
fn lone_entry(route: &Route) -> Option<&String> {
    let mut entries = route.request_models.values();
    let only = entries.next()?;
    entries.next().is_none().then_some(only)
}

impl PendingCheck {
    /// [`PendingCheck::rejection`] computed OFF the async worker, within
    /// [`CHECK_DEADLINE`]: the answer the frame-serving task awaits.
    ///
    /// Parsing and compiling a schema is synchronous CPU work, and a serve path that does it
    /// inline has nowhere for a cancellation, a timeout or a shutdown to land while it runs —
    /// so it is handed to the blocking pool and awaited with a deadline. A check that does not
    /// finish within it refuses NOTHING: the frame is served exactly as it would be for a
    /// route whose schema states nothing checkable, and the reason stays in this emulator's
    /// own log. The work itself is bounded before it starts ([`MAX_SCHEMA_BYTES`] and the
    /// socket's per-message ceiling), so reaching the deadline is not something a caller can
    /// drive by sending more, or bigger, frames.
    pub async fn rejected(self, body: String) -> Option<String> {
        let model = self.model_name.clone();
        let checked = ::tokio::task::spawn_blocking(move || self.rejection(&body));
        match ::tokio::time::timeout(CHECK_DEADLINE, checked).await {
            Ok(Ok(rejection)) => rejection,
            Ok(Err(joined)) => {
                ::tracing::warn!(
                    %model,
                    error = %joined,
                    "this route's model check did not complete, so the message it was about \
                     is checked against nothing"
                );
                None
            }
            Err(_elapsed) => {
                ::tracing::warn!(
                    %model,
                    deadline_ms = CHECK_DEADLINE.as_millis(),
                    "this route's model check did not answer within its deadline, so the \
                     message it was about is checked against nothing"
                );
                None
            }
        }
    }

    /// Why this message does not satisfy the model, or `None` when it does — or when the
    /// schema states nothing this emulator can check it against, which refuses nothing.
    ///
    /// The sentence is for this emulator's own log: the client is answered
    /// [`INVALID_BODY_MESSAGE`], the frame API Gateway refuses such a message with.
    ///
    /// Synchronous: every caller of it on a serve path goes through [`PendingCheck::rejected`],
    /// which runs it off the async worker.
    fn rejection(&self, body: &str) -> Option<String> {
        let compiled = ::serde_json::from_str::<Value>(&self.schema).ok()?;
        // A JSON Schema is applied to a JSON document, so a message that is not one does not
        // satisfy the model at all — the same reading the REST data plane's request validator
        // takes of an unreadable payload.
        let Ok(instance) = ::serde_json::from_str::<Value>(body) else {
            return Some("the message is not a JSON document".to_string());
        };
        let mut compiler = ::boon::Compiler::new();
        // The only schema resource is the one registered below: a model's schema is a
        // document any caller stores and `$ref` is a dynamic resolution mechanism, so
        // replacing the compiler's own loader is what keeps a reference from reaching the
        // host's filesystem or network ([`NoSchemaBeyondTheModel`]).
        compiler.use_loader(Box::new(NoSchemaBeyondTheModel));
        // "For application/json models, this should be JSON schema draft 4 model"
        // (`Model$Schema`), so draft 4 is the dialect a schema declaring no `$schema` of its
        // own is read under; one that declares a dialect keeps it.
        compiler.set_default_draft(::boon::Draft::V4);
        let resource = model_schema_resource(&self.model_name);
        if compiler.add_resource(&resource, compiled).is_err() {
            return None;
        }
        let mut schemas = ::boon::Schemas::new();
        let Ok(schema) = compiler.compile(&resource, &mut schemas) else {
            return None;
        };
        schemas
            .validate(&instance, schema)
            .err()
            .map(|failed| one_line(&failed.to_string()))
    }
}

/// The identifier the validator keys ONE model's schema document by. It is not an address:
/// nothing is fetched from it, and [`NoSchemaBeyondTheModel`] is what every other reference
/// resolves through.
fn model_schema_resource(model_name: &str) -> String {
    format!("https://apigateway.amazonaws.com/models/{model_name}")
}

/// The schema resources this validator resolves a `$ref` against: NOTHING but the one
/// document [`PendingCheck::rejection`] registered itself.
///
/// A model's schema is a document any caller stores, so a `$ref` in it could name a resource
/// by any URL — including a `file:` one. Registering this as the compiler's loader REPLACES
/// the one it comes with, so such a reference resolves to an error instead of reaching the
/// host's filesystem or network; the schema then compiles to nothing, which checks the frame
/// against nothing rather than refusing it.
#[derive(Debug)]
struct NoSchemaBeyondTheModel;

impl ::boon::UrlLoader for NoSchemaBeyondTheModel {
    fn load(&self, url: &str) -> Result<Value, Box<dyn ::std::error::Error>> {
        ::tracing::warn!(
            reference = %url,
            "this route's model references a schema resource outside its own document, which \
             this emulator resolves nothing for"
        );
        Err("this emulator resolves no schema outside the model's own document".into())
    }
}

/// The validator's own report, flattened to ONE line for the log: which member of the message
/// failed which keyword of the model's schema.
fn one_line(reported: &str) -> String {
    reported
        .split_whitespace()
        .filter(|word| *word != "-")
        .collect::<Vec<&str>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::state::Model;

    /// A route naming `models` with the selection expression `expression`.
    fn route(expression: Option<&str>, models: &[(&str, &str)]) -> Route {
        Route {
            route_key: "sendmsg".to_string(),
            model_selection_expression: expression.map(str::to_string),
            request_models: models
                .iter()
                .map(|(key, name)| ((*key).to_string(), (*name).to_string()))
                .collect(),
            ..Route::default()
        }
    }

    /// A deployment holding one model named `name` with the schema `schema`.
    fn deployed(name: &str, schema: &str) -> DeployedConfiguration {
        let mut models = BTreeMap::new();
        models.insert(
            "mdl0000001".to_string(),
            Model {
                model_id: "mdl0000001".to_string(),
                name: name.to_string(),
                content_type: Some("application/json".to_string()),
                description: None,
                schema: schema.to_string(),
            },
        );
        DeployedConfiguration {
            models,
            ..DeployedConfiguration::default()
        }
    }

    /// The schema the rows below check a message against: one required string member.
    const SCHEMA: &str = r#"{
        "$schema": "http://json-schema.org/draft-04/schema#",
        "title": "Message",
        "type": "object",
        "required": ["text"],
        "properties": { "text": { "type": "string" } }
    }"#;

    /// One row: what a failing assertion calls it, the route's `ModelSelectionExpression`,
    /// its `RequestModels`, the name of the one model the stage HOLDS, and the model the
    /// frame is checked against.
    type Row = (
        &'static str,
        Option<&'static str>,
        &'static [(&'static str, &'static str)],
        &'static str,
        Option<&'static str>,
    );

    #[test]
    fn the_model_a_route_selects_is_the_one_the_frame_is_checked_against() {
        // The rows differ along HOW the route states which model keys the message: the
        // `$default` entry alone, an expression naming that key, an expression naming a path
        // into the body whose value keys the map, a single entry under some other key, a key
        // nothing in the map holds, and a route naming a model the stage does not hold.
        let cases: [Row; 6] = [
            ("the $default entry alone", None, &[("$default", "Message")], "Message", Some("Message")),
            (
                "an expression naming the $default key",
                Some("$default"),
                &[("$default", "Message")],
                "Message",
                Some("Message"),
            ),
            (
                "an expression naming a path into the body",
                Some("$request.body.action"),
                &[("sendmsg", "Message")],
                "Message",
                Some("Message"),
            ),
            (
                "a single entry under another key",
                None,
                &[("application/json", "Message")],
                "Message",
                Some("Message"),
            ),
            (
                "an expression whose value keys no entry of a map holding several",
                Some("$request.body.action"),
                &[("other", "Message"), ("another", "Second")],
                "Message",
                None,
            ),
            (
                "a model the stage's deployment does not hold",
                None,
                &[("$default", "Elsewhere")],
                "Message",
                None,
            ),
        ];
        for (case, expression, models, held, expected) in cases {
            let pending = pending(
                &deployed(held, SCHEMA),
                &route(expression, models),
                r#"{"action": "sendmsg", "text": "hello"}"#,
            );
            let actual = pending.as_ref().map(|check| check.model_name.as_str());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the model {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn a_message_is_refused_only_where_the_model_states_something_it_fails() {
        // The rows differ along the MESSAGE and along the schema it is checked against: a
        // body that satisfies the model, one whose member is of another type, one missing a
        // required member, one that is no JSON document at all, and the two schemas that state
        // nothing checkable — a document that is no JSON Schema, and one whose `$ref` names a
        // resource outside itself.
        let cases: [(&str, &str, &str, bool); 7] = [
            ("a body the model admits", SCHEMA, r#"{"text": "hello"}"#, false),
            (
                "a member of another type",
                SCHEMA,
                r#"{"action": "sendmsg", "text": 12345}"#,
                true,
            ),
            ("a missing required member", SCHEMA, r#"{"action": "sendmsg"}"#, true),
            ("a body that is not JSON at all", SCHEMA, "not json", true),
            (
                "a body of the wrong top-level type",
                SCHEMA,
                r#"["text"]"#,
                true,
            ),
            ("a schema that is no JSON document", "{ not a schema", r#"{"text": 1}"#, false),
            (
                "a schema whose reference names a resource outside itself",
                r#"{"$ref": "https://elsewhere.test/schema"}"#,
                r#"{"text": 1}"#,
                false,
            ),
        ];
        for (case, schema, body, refused) in cases {
            let check = PendingCheck {
                model_name: "Message".to_string(),
                schema: schema.to_string(),
            };
            let rejection = check.rejection(body);
            assert_eq!(
                rejection.is_some(),
                refused,
                "case {case:?}: expected refused {refused}, got {rejection:?}"
            );
        }
    }

    /// A schema document of exactly `bytes` bytes: a JSON Schema whose `description` is padded
    /// out to the length the row asks for, so the only thing a row varies is the LENGTH.
    fn schema_of_length(bytes: usize) -> String {
        let shell = r#"{"type":"object","description":""}"#;
        let padding = bytes.saturating_sub(shell.len());
        format!(
            r#"{{"type":"object","description":"{}"}}"#,
            "p".repeat(padding)
        )
    }

    #[test]
    fn a_schema_longer_than_a_model_may_be_is_checked_against_nothing() {
        // The rows walk the published bound and one byte past it: the length
        // `CreateModel$Schema` declares ("A string with a length between [0-32768]") is the
        // longest document one frame's check may be made to parse and compile.
        let cases: [(&str, usize, bool); 3] = [
            ("a schema well inside the bound", 1_024, true),
            ("a schema at the bound", MAX_SCHEMA_BYTES, true),
            ("a schema one byte past the bound", MAX_SCHEMA_BYTES + 1, false),
        ];
        for (case, bytes, applied) in cases {
            let schema = schema_of_length(bytes);
            assert_eq!(
                schema.len(),
                bytes,
                "case {case:?}: the row's own schema must be {bytes} bytes, got {}",
                schema.len()
            );
            let check = pending(
                &deployed("Message", &schema),
                &route(None, &[("$default", "Message")]),
                r#"{"action": "sendmsg", "text": "hello"}"#,
            );
            assert_eq!(
                check.is_some(),
                applied,
                "case {case:?}: expected the model to be applied {applied}, got {check:?}"
            );
        }
    }
}
