// SPDX-License-Identifier: CC0-1.0

//! What a WEBSOCKET route's NON-PROXY integration answers: the request template that is
//! rendered for the backend, the pass-through behaviour of a call no template keys, and the
//! response template the backend's answer is read through.
//!
//! [`super::integration`] performs the type that passes a call through as-is (`AWS_PROXY`,
//! the Lambda proxy integration) and [`super::dispatch`] the one that reaches no backend at
//! all (`MOCK`). This module is the third: `AWS`, "for integrating the route or method
//! request with an AWS service action, including the Lambda function-invoking action … also
//! referred to as the HTTP custom integration" (`Integration$IntegrationType`), where what
//! the backend receives is NOT the call's event but what the integration's own mapping
//! template renders.
//!
//! The rule, from the three members that describe it:
//!
//! 1. WHICH template keys the call. "The template selection expression for the integration.
//!    Supported only for WebSocket APIs" (`Integration$TemplateSelectionExpression`) — so the
//!    key is what that expression evaluates to over this call ([`selection_key`]), and the
//!    content type a WebSocket frame carries ([`super::dispatch::JSON_TEMPLATE_KEY`]) for an
//!    integration that declares no expression. The entry under THAT key and no other: the
//!    request leg keys strictly, where [`super::dispatch::keyed_template`] (the reading a
//!    `MOCK` integration's single template and the response leg below take) falls back to the
//!    one entry a map holds whatever its key. Rule 2 is what makes the difference matter —
//!    `NEVER` and `WHEN_NO_TEMPLATES` exist to refuse a call the integration maps no template
//!    FOR THIS CONTENT TYPE, which a fallback to "the only entry there is" would serve
//!    instead;
//! 2. what happens when `RequestTemplates` holds NO template under that key.
//!    `Integration$PassthroughBehavior` "specifies the pass-through behavior for incoming
//!    requests based on the Content-Type header in the request, and the available mapping
//!    templates specified as the requestTemplates property": "WHEN_NO_MATCH passes the
//!    request body for unmapped content types through to the integration backend without
//!    transformation. NEVER rejects unmapped content types … WHEN_NO_TEMPLATES allows
//!    pass-through when the integration has no content types mapped to templates" — and
//!    refuses the call when it has at least one. A call that is refused reaches NO backend,
//!    and the client reads the gateway's own `Internal server error` for it, which is what
//!    this data plane answers every call whose route produced no reply;
//! 3. what the client reads back. The backend's answer selects one of the integration's
//!    `IntegrationResponses` by the status the call produced, and that response's own
//!    `ResponseTemplates` entry is rendered over the backend's answer — the same two steps a
//!    `MOCK` integration's reply follows ([`super::dispatch::mock_reply`]), with a real
//!    backend's answer in place of a rendered status. An integration holding no integration
//!    response at all maps nothing, so the backend's own answer is what the client reads.
//!
//! The BACKEND is reached the one way this data plane reaches one: an `AWS` integration of a
//! WebSocket api names an action's resource in its `IntegrationUri`, and the action this
//! emulator performs over a socket is Lambda's own `Invoke` ([`super::super::invoke::lambda`],
//! the same in-process hop every other Lambda-backed step of this service makes). A URI
//! naming anything else names a backend this data plane does not call, which is reported as
//! unperformable rather than answered as if something had run.

use std::collections::BTreeMap;

use ::smithy_server_runtime::EmbeddedEmulator;

use super::dispatch::{self, TemplateCall, Unserved, JSON_TEMPLATE_KEY};
use super::event::{self, Call, Connection};
use super::integration::{Answered, Selected};
use crate::service::invoke::lambda;
use crate::state::Integration;

/// The status an invocation the backend ANSWERED produced: the call of the service action
/// succeeded, which is what the integration reports to the selection of its integration
/// responses. A non-proxy integration's backend states no status of its own — unlike a proxy
/// integration, whose function answers one — so what selects the response is the status of
/// the CALL the gateway made.
const ANSWERED_STATUS: u16 = 200;

/// What `PassthroughBehavior` each documented value is, lowercased comparisons aside: the
/// three values `Integration$PassthroughBehavior` enumerates.
const WHEN_NO_MATCH: &str = "WHEN_NO_MATCH";
const WHEN_NO_TEMPLATES: &str = "WHEN_NO_TEMPLATES";
const NEVER: &str = "NEVER";

/// Perform `integration` — an `AWS` (non-proxy) integration of a WebSocket route — for one
/// `call` of `connection`, and answer what it replied.
pub async fn perform(
    callback: &EmbeddedEmulator,
    connection: &Connection,
    selected: &Selected,
    integration: &Integration,
    call: &Call,
    request_id: &str,
) -> Result<Answered, Unserved> {
    let context = event::template_context_of(connection, &selected.route_key, call, request_id);
    let frame = match call {
        Call::Message(body) => Some(body.as_str()),
        // Neither the connection request nor the close carries a frame, so a template
        // rendered for one reads the call's context and nothing of a payload.
        Call::Connect | Call::Disconnect { .. } => None,
    };
    // The frame is read as the document it is ONCE for the whole call, however many
    // references the integration's selection expression makes into it.
    let document = frame.and_then(|frame| ::serde_json::from_str::<::serde_json::Value>(frame).ok());
    let template_call = TemplateCall::new(&context, &connection.stage_variables, frame);
    let body = request_body(
        integration,
        &context,
        connection,
        frame,
        document.as_ref(),
        template_call,
    )?;
    let target = backend(integration, connection)?;
    // The invocation is made with the RENDERED body as the payload: a non-proxy integration
    // sends the backend what its template produced, which is the whole point of the type.
    let ran = lambda::invoke_function(
        callback,
        &target,
        body.into_bytes(),
        super::integration::deadline(integration),
    )
    .await
    .map_err(|unreached| Unserved::Undelivered(unreached.reason()))?;
    if ran.failed() {
        return Err(Unserved::FunctionError(format!(
            "this route's backend ended in an unhandled error ({}), so the integration \
             produced no reply",
            ran.function_error.as_deref().unwrap_or_default()
        )));
    }
    let answered = String::from_utf8_lossy(&ran.payload).to_string();
    let payload = reply_payload(integration, &context, connection, &answered)?;
    Ok(Answered {
        status_code: Some(ANSWERED_STATUS.to_string()),
        payload: (!payload.is_empty()).then(|| payload.into_bytes()),
    })
}

/// The body this call is relayed to the backend with: the rendering of the template that keys
/// the call, and the frame itself where the integration's `PassthroughBehavior` passes an
/// unmapped call through.
///
/// The template is the entry under the key the call selects and no other — see rule 1 in the
/// module docs for why the request leg keys strictly where the other readings fall back.
///
/// `Err` is the refusal of a call that keys no template and is not passed through — the
/// `NEVER` behaviour, and `WHEN_NO_TEMPLATES` on an integration that has at least one
/// template — so nothing is invoked for it.
fn request_body(
    integration: &Integration,
    context: &BTreeMap<String, String>,
    connection: &Connection,
    frame: Option<&str>,
    document: Option<&::serde_json::Value>,
    template_call: TemplateCall<'_>,
) -> Result<String, Unserved> {
    let key = selection_key(
        integration.template_selection_expression.as_deref(),
        context,
        &connection.stage_variables,
        document,
    );
    if let Some(template) = key
        .as_deref()
        .and_then(|key| integration.request_templates.get(key))
    {
        return dispatch::rendered_template(template, Some(template_call));
    }
    if passes_through(integration) {
        // "WHEN_NO_MATCH passes the request body for unmapped content types through to the
        // integration backend without transformation": what the backend receives is the
        // frame as the client sent it.
        return Ok(frame.unwrap_or_default().to_string());
    }
    Err(Unserved::UnsupportedMediaType(format!(
        "this route's integration maps no request template for {}, and its \
         PassthroughBehavior does not pass an unmapped call through, so nothing was relayed \
         to the backend",
        key.as_deref().unwrap_or("this call")
    )))
}

/// Whether an unmapped call is passed through to the backend, by this integration's
/// `PassthroughBehavior`: `WHEN_NO_MATCH` always, `WHEN_NO_TEMPLATES` only for an integration
/// that maps no template at all, and `NEVER` not at all.
///
/// An integration carrying no value is read as the one the writers default it to
/// ([`crate::state::DEFAULT_PASSTHROUGH_BEHAVIOR`]), and a value that is none of the three
/// passes nothing through: a behaviour this does not know is not one to forward a call under.
fn passes_through(integration: &Integration) -> bool {
    let behavior = integration
        .passthrough_behavior
        .as_deref()
        .map(str::trim)
        .filter(|behavior| !behavior.is_empty())
        .unwrap_or(crate::state::DEFAULT_PASSTHROUGH_BEHAVIOR);
    if behavior.eq_ignore_ascii_case(WHEN_NO_MATCH) {
        return true;
    }
    if behavior.eq_ignore_ascii_case(WHEN_NO_TEMPLATES) {
        return integration.request_templates.is_empty();
    }
    if !behavior.eq_ignore_ascii_case(NEVER) {
        ::tracing::debug!(
            passthrough_behavior = %behavior,
            "this integration names a pass-through behaviour this gateway does not know, so \
             an unmapped call is not passed through"
        );
    }
    false
}

/// The key the call selects a request template by, or `None` when the integration's
/// `TemplateSelectionExpression` names a part of the call that has no value — which selects
/// no template at all and leaves the pass-through behaviour to decide.
///
/// The expression is evaluated by the service's ONE reader of a selection expression
/// (`crate::state::evaluated_selection_expression`), so an escaped key (`\$default`) and a
/// reference into the call are read here exactly as every other reader of one reads them.
/// An integration that declares no expression keys its templates by the call's own content
/// type, which for a WebSocket frame is [`JSON_TEMPLATE_KEY`].
fn selection_key(
    expression: Option<&str>,
    context: &BTreeMap<String, String>,
    stage_variables: &BTreeMap<String, String>,
    frame: Option<&::serde_json::Value>,
) -> Option<String> {
    let Some(expression) = expression.map(str::trim).filter(|value| !value.is_empty()) else {
        return Some(JSON_TEMPLATE_KEY.to_string());
    };
    crate::state::evaluated_selection_expression(expression, &|reference| {
        selected_value(reference, context, stage_variables, frame)
    })
}

/// The value one reference of a selection expression has for this call: a field of the
/// message body, a `$context` member the gateway reports about the call, or a variable of the
/// serving stage — WHICH of those a reference names being
/// `crate::state::selection_reference`'s reading, the one owner of that set.
///
/// `frame` is the message PARSED ONCE for the whole call, so an expression making several
/// references costs one reading of the client's frame and not one per reference.
fn selected_value(
    reference: &str,
    context: &BTreeMap<String, String>,
    stage_variables: &BTreeMap<String, String>,
    frame: Option<&::serde_json::Value>,
) -> Option<String> {
    let (namespace, name) = crate::state::selection_reference(reference)?;
    match namespace {
        crate::state::SelectionNamespace::RequestBody => {
            crate::state::selected_body_value(frame?, name)
        }
        crate::state::SelectionNamespace::Context => context.get(name).cloned(),
        crate::state::SelectionNamespace::StageVariables => stage_variables.get(name).cloned(),
    }
}

/// The payload the client reads back: the rendering of the selected integration response's
/// own `ResponseTemplates` entry, and the backend's own answer for an integration that holds
/// no integration response at all.
///
/// `Err` when the integration DOES hold integration responses and the status the call
/// produced selects none of them: API Gateway then has no mapping to answer from, which is a
/// fault of the api's configuration.
fn reply_payload(
    integration: &Integration,
    context: &BTreeMap<String, String>,
    connection: &Connection,
    answered: &str,
) -> Result<String, Unserved> {
    if integration.integration_responses.is_empty() {
        return Ok(answered.to_string());
    }
    let status = ANSWERED_STATUS.to_string();
    let selected = dispatch::selected_integration_response(integration, Some(&status))
        .ok_or(Unserved::NoSelectableResponse)?;
    // The response template reads the BACKEND's answer as `$input`: that is the payload being
    // mapped here, where a request template reads the frame. WHICH entry of a template map
    // keys the call is `dispatch::keyed_template`'s reading, the one owner of it.
    let call = TemplateCall::new(context, &connection.stage_variables, Some(answered));
    let Some(template) = dispatch::keyed_template(&selected.response_templates) else {
        return Ok(answered.to_string());
    };
    dispatch::rendered_template(template, Some(call))
}

/// The function the integration's `IntegrationUri` names, with this stage's variables
/// interpolated into it first — the same resolution every other backend of this data plane is
/// addressed through.
fn backend(
    integration: &Integration,
    connection: &Connection,
) -> Result<lambda::FunctionTarget, Unserved> {
    let uri = integration
        .integration_uri
        .as_deref()
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
        .and_then(|uri| {
            crate::service::data_plane::resolve_stage_variables(uri, &connection.stage_variables)
        })
        .ok_or_else(|| {
            Unserved::Unperformable(
                "this route's integration names no backend this stage can reach".to_string(),
            )
        })?;
    lambda::function_target(&uri).ok_or_else(|| {
        Unserved::Unperformable(
            "this route's integration names an action this data plane does not perform over a \
             socket"
                .to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context and stage variables the rows below are evaluated against.
    fn context() -> BTreeMap<String, String> {
        BTreeMap::from([("routeKey".to_string(), "send".to_string())])
    }

    fn stage_variables() -> BTreeMap<String, String> {
        BTreeMap::from([("tenant".to_string(), "acme".to_string())])
    }

    #[test]
    fn the_selection_expression_states_which_template_keys_the_call() {
        let context = context();
        let variables = stage_variables();
        let frame: ::serde_json::Value =
            ::serde_json::from_str(r#"{"action":"send","count":2}"#).expect("a JSON frame");
        // Rows differ along the EXPRESSION: no expression at all, the escaped literal the
        // console writes, each namespace a selection expression may read, both spellings of a
        // reference, a non-string body field, and a reference this call has no value for.
        let cases: [(&str, Option<&str>, Option<&str>); 8] = [
            ("no expression, so the frame's own content type", None, Some(JSON_TEMPLATE_KEY)),
            ("the escaped literal key", Some(r"\$default"), Some("$default")),
            ("a field of the message body", Some("$request.body.action"), Some("send")),
            ("the braced spelling", Some("${request.body.action}"), Some("send")),
            (
                "a body field that is no string",
                Some("$request.body.count"),
                Some("2"),
            ),
            ("a member of the call's context", Some("$context.routeKey"), Some("send")),
            ("a variable of the serving stage", Some("$stageVariables.tenant"), Some("acme")),
            (
                "a reference this call has no value for",
                Some("$request.body.absent"),
                None,
            ),
        ];
        for (case, expression, expected) in cases {
            let key = selection_key(expression, &context, &variables, Some(&frame));
            assert_eq!(
                key.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {key:?}"
            );
        }
    }

    #[test]
    fn the_passthrough_behavior_decides_an_unmapped_call() {
        // Rows span the three documented values, each over an integration that maps a
        // template and one that maps none, plus the value the writers default to.
        let cases: [(&str, Option<&str>, bool, bool); 7] = [
            ("WHEN_NO_MATCH with a template mapped", Some(WHEN_NO_MATCH), true, true),
            ("WHEN_NO_MATCH with none mapped", Some(WHEN_NO_MATCH), false, true),
            (
                "WHEN_NO_TEMPLATES with a template mapped",
                Some(WHEN_NO_TEMPLATES),
                true,
                false,
            ),
            (
                "WHEN_NO_TEMPLATES with none mapped",
                Some(WHEN_NO_TEMPLATES),
                false,
                true,
            ),
            ("NEVER with a template mapped", Some(NEVER), true, false),
            ("NEVER with none mapped", Some(NEVER), false, false),
            ("an integration carrying no behaviour", None, true, true),
        ];
        for (case, behavior, maps_a_template, expected) in cases {
            let integration = Integration {
                passthrough_behavior: behavior.map(str::to_string),
                request_templates: if maps_a_template {
                    BTreeMap::from([("application/xml".to_string(), "x".to_string())])
                } else {
                    BTreeMap::new()
                },
                ..Integration::default()
            };
            let passes = passes_through(&integration);
            assert_eq!(
                passes, expected,
                "case {case:?}: expected passes_through {expected}, got {passes}"
            );
        }
    }
}
