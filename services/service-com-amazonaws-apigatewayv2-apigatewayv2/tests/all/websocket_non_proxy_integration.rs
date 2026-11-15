// SPDX-License-Identifier: CC0-1.0

//! A WEBSOCKET route served by a NON-PROXY (`AWS`) integration, over a real connection: the
//! request template the call selects, what happens to a call no template keys, and the
//! response template the backend's answer is read back through.
//!
//! The rule the tables below are each one slice of: **an `AWS` integration of a WebSocket
//! route relays to its backend what its own `RequestTemplates` entry RENDERS — the entry the
//! integration's `TemplateSelectionExpression` keys, or the one keyed by the content type a
//! frame carries where it declares no expression — and a call that keys no template is
//! relayed only where the integration's `PassthroughBehavior` passes an unmapped call
//! through ("WHEN_NO_MATCH passes the request body for unmapped content types through to the
//! integration backend without transformation. NEVER rejects unmapped content types …
//! WHEN_NO_TEMPLATES allows pass-through when the integration has no content types mapped to
//! templates", `Integration$PassthroughBehavior`), with every other such call refused and
//! answered the gateway's own frame; what the client reads back is the `ResponseTemplates`
//! entry of the `IntegrationResponse` the call's status selected, and the backend's own
//! answer where the integration holds no integration response at all.**
//!
//! Nothing here is a value this emulator fills in: every assertion is on what a function RAN
//! and returned for the payload the gateway relayed to it, which is why AWS Lambda is mounted
//! beside this service on one engine.
//!
//! The suites that own the steps BEFORE the integration are not restated here: the
//! handshake's own statuses, the route selection and the `MOCK` reply rule are
//! `websocket_transport_data_plane.rs`, and an `AWS_PROXY` route's own event is
//! `websocket_lambda_routes.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{emulator_with_lambda, given_function};
use crate::websocket_transport_data_plane::{
    create_api, create_integration, create_integration_response, create_route, create_stage,
    exchanged_reply, handshake_at, ClientSocket,
};

/// The Region the backend function lives in.
const REGION: &str = "us-east-1";

/// The stage every case dials.
const STAGE: &str = "prod";

/// The route key every case's frames select, and the frame they send.
const ROUTE_KEY: &str = "send";
const FRAME: &str = r#"{"action":"send","say":"hello"}"#;

/// The template every case renders: a document naming the route the call was served by and
/// the member of the frame the template reads, so what the backend received proves the
/// template was RENDERED rather than the frame passed through.
const TEMPLATE: &str = r#"{"routed":"$context.routeKey","said":$input.json('$.say')}"#;

/// The sentence the gateway's own frame carries for a call its route produced no reply for.
const INTERNAL_MESSAGE: &str = "Internal server error";

/// A backend that answers the payload it was given, which is how what the gateway relayed is
/// asserted. A non-proxy integration's backend is called with the rendered body, so the
/// payload IS the event this handler receives.
const ECHO_PAYLOAD: &str = "def lambda_handler(event, context):\n    return {'received': event}\n";

/// The `IntegrationUri` an `AWS` integration names a function to invoke with: the path form
/// AWS documents for the Lambda function-invoking action.
fn lambda_uri(function_arn: &str) -> String {
    format!(
        "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/invocations"
    )
}

/// An `AWS` (non-proxy) integration over `function_arn`, carrying `request_templates` and the
/// two members that describe how a call selects one of them.
fn aws_integration(
    function_arn: &str,
    request_templates: Value,
    selection_expression: Option<&str>,
    passthrough_behavior: Option<&str>,
) -> Value {
    let mut integration = json!({
        "integrationType": "AWS",
        "integrationUri": lambda_uri(function_arn),
        "integrationMethod": "POST",
        "requestTemplates": request_templates
    });
    if let Some(expression) = selection_expression {
        integration["templateSelectionExpression"] = json!(expression);
    }
    if let Some(behavior) = passthrough_behavior {
        integration["passthroughBehavior"] = json!(behavior);
    }
    integration
}

/// One api serving [`ROUTE_KEY`] with `integration` on a deployed [`STAGE`], with
/// `integration_response` created for it when there is one. Answers the socket a client
/// dials and the `IntegrationId`.
async fn given_api(
    emulator: &MultiServiceRouter,
    case: &str,
    name: &str,
    integration: Value,
    integration_response: Option<Value>,
) -> (String, String) {
    let api = create_api(emulator, case, name).await;
    let integration_id = create_integration(emulator, case, &api.api_id, integration).await;
    if let Some(response) = integration_response {
        create_integration_response(emulator, case, &api.api_id, &integration_id, response).await;
    }
    // A two-way route: "to define a two-way route, create a route response", which is what
    // makes the reply reach the client.
    create_route(emulator, case, &api.api_id, ROUTE_KEY, &integration_id, true).await;
    create_stage(emulator, case, &api.api_id, STAGE, true).await;
    (api.authority, integration_id)
}

/// Dial `authority`, send [`FRAME`] and answer the frame the api replied with.
///
/// Waiting for that reply is the harness's own reading
/// ([`crate::websocket_transport_data_plane::exchanged_reply`]): the FIRST invocation of a
/// function starts its runtime, so the wait is for the api's REPLY rather than for a fixed
/// time. Every route below answers the same reply to the same frame, so asking again cannot
/// change what a row reads.
async fn replied(case: &str, authority: &str) -> String {
    let mut socket: ClientSocket = handshake_at(case, authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: the handshake must be completed, got {status}")
        });
    exchanged_reply(case, &mut socket, FRAME).await
}

/// The frame the api replied with, as the JSON document it is.
fn document(case: &str, reply: &str) -> Value {
    serde_json::from_str(reply)
        .unwrap_or_else(|error| panic!("case {case:?}: the reply must be JSON, got {reply:?}: {error}"))
}

#[tokio::test]
async fn an_aws_integration_relays_the_template_the_call_selects() {
    let (emulator, engine) = emulator_with_lambda();
    let case = "an AWS integration whose request template renders the call";
    let backend = given_function(&emulator, &engine, case, "template-backend", ECHO_PAYLOAD).await;

    /// One row: how the integration keys its templates, and the selection expression it
    /// declares.
    type Row = (&'static str, Value, Option<&'static str>);
    // Rows differ along HOW the call selects the template: the escaped literal key an api's
    // console writes, the content type a frame carries where no expression is declared, and
    // an expression read off the message body itself.
    let rows: [Row; 3] = [
        (
            "the escaped literal selection key",
            json!({ "$default": TEMPLATE }),
            Some(r"\$default"),
        ),
        (
            "no selection expression, so the frame's own content type",
            json!({ "application/json": TEMPLATE }),
            None,
        ),
        (
            "a selection expression read off the message body",
            json!({ "send": TEMPLATE }),
            Some("$request.body.action"),
        ),
    ];
    for (index, (row, templates, expression)) in rows.into_iter().enumerate() {
        let (authority, _) = given_api(
            &emulator,
            row,
            &format!("non-proxy-template-{index}"),
            aws_integration(&backend, templates, expression, None),
            None,
        )
        .await;
        let reply = replied(row, &authority).await;
        let answered = document(row, &reply);
        // The backend received what the TEMPLATE rendered: the route it was served by and
        // the member of the frame the template read, and nothing else of the frame.
        assert_eq!(
            answered.pointer("/received/routed").and_then(Value::as_str),
            Some(ROUTE_KEY),
            "case {row:?}: expected the rendered template to reach the backend, got {reply}"
        );
        assert_eq!(
            answered.pointer("/received/said").and_then(Value::as_str),
            Some("hello"),
            "case {row:?}: expected the template's own member, got {reply}"
        );
        assert_eq!(
            answered.pointer("/received/action"),
            None,
            "case {row:?}: a rendered template replaces the frame, so the frame's own \
             members must not reach the backend, got {reply}"
        );
    }
}

#[tokio::test]
async fn an_unmapped_call_follows_the_integrations_passthrough_behavior() {
    let (emulator, engine) = emulator_with_lambda();
    let case = "an AWS integration whose templates key no content type the call carries";
    let backend = given_function(&emulator, &engine, case, "passthrough-backend", ECHO_PAYLOAD)
        .await;

    /// One row: the behaviour declared, whether the integration maps a template the call does
    /// not key, and whether the call must reach the backend.
    type Row = (&'static str, Option<&'static str>, bool, bool);
    // Rows span the three documented behaviours, each over an integration that maps a
    // template the call does not key and one that maps none at all, plus the behaviour an
    // integration that declares none carries.
    let rows: [Row; 5] = [
        ("NEVER with a template the call does not key", Some("NEVER"), true, false),
        (
            "WHEN_NO_TEMPLATES with a template the call does not key",
            Some("WHEN_NO_TEMPLATES"),
            true,
            false,
        ),
        (
            "WHEN_NO_TEMPLATES with no template at all",
            Some("WHEN_NO_TEMPLATES"),
            false,
            true,
        ),
        (
            "WHEN_NO_MATCH with a template the call does not key",
            Some("WHEN_NO_MATCH"),
            true,
            true,
        ),
        ("an integration declaring no behaviour", None, true, true),
    ];
    for (index, (row, behavior, maps_a_template, reaches_the_backend)) in
        rows.into_iter().enumerate()
    {
        // A template keyed by a content type a WebSocket frame does not carry, so the call
        // keys none of this integration's templates.
        let templates = if maps_a_template {
            json!({ "application/xml": TEMPLATE })
        } else {
            json!({})
        };
        let (authority, _) = given_api(
            &emulator,
            row,
            &format!("non-proxy-passthrough-{index}"),
            aws_integration(&backend, templates, None, behavior),
            None,
        )
        .await;
        let reply = replied(row, &authority).await;
        let answered = document(row, &reply);
        if reaches_the_backend {
            // Passed through "without transformation": the backend received the frame
            // exactly as the client sent it.
            assert_eq!(
                answered.get("received"),
                Some(&document(row, FRAME)),
                "case {row:?}: expected the frame itself to reach the backend, got {reply}"
            );
        } else {
            assert_eq!(
                answered.get("message").and_then(Value::as_str),
                Some(INTERNAL_MESSAGE),
                "case {row:?}: expected the gateway's own frame for a refused call, got {reply}"
            );
            assert_eq!(
                answered.get("received"),
                None,
                "case {row:?}: a refused call reaches no backend, got {reply}"
            );
        }
    }
}

#[tokio::test]
async fn the_selected_integration_response_maps_what_the_client_reads() {
    let (emulator, engine) = emulator_with_lambda();

    /// One row: the key of the integration response the integration holds, the template it
    /// renders, and what the client must read back — `None` when the call selects no
    /// response at all, which is the gateway's own frame.
    type Row = (&'static str, &'static str, Option<&'static str>, Option<&'static str>);
    // Rows differ along the key the integration response carries against the status the call
    // produced: a pattern that matches it, the `$default` fallback, a response holding no
    // template, and a pattern that matches nothing.
    let rows: [Row; 4] = [
        (
            "a patterned key that matches the call's status",
            r"/2\d\d/",
            Some(r#"{"wrapped":$input.body}"#),
            Some("hello"),
        ),
        (
            "the $default fallback",
            "$default",
            Some(r#"{"wrapped":$input.body}"#),
            Some("hello"),
        ),
        (
            "a selected response holding no template, which maps nothing",
            "$default",
            None,
            None,
        ),
        (
            "a patterned key the call's status does not match",
            r"/4\d\d/",
            Some(r#"{"wrapped":$input.body}"#),
            None,
        ),
    ];
    for (index, (row, key, response_template, wrapped)) in rows.into_iter().enumerate() {
        let backend = given_function(
            &emulator,
            &engine,
            row,
            &format!("response-backend-{index}"),
            ECHO_PAYLOAD,
        )
        .await;
        let mut response = json!({ "integrationResponseKey": key });
        if let Some(template) = response_template {
            response["responseTemplates"] = json!({ "$default": template });
        }
        let (authority, _) = given_api(
            &emulator,
            row,
            &format!("non-proxy-response-{index}"),
            aws_integration(
                &backend,
                json!({ "$default": TEMPLATE }),
                Some(r"\$default"),
                None,
            ),
            Some(response),
        )
        .await;
        let reply = replied(row, &authority).await;
        let answered = document(row, &reply);
        match wrapped {
            Some(said) => assert_eq!(
                answered
                    .pointer("/wrapped/received/said")
                    .and_then(Value::as_str),
                Some(said),
                "case {row:?}: expected the response template to wrap the backend's answer, \
                 got {reply}"
            ),
            None if response_template.is_none() => assert_eq!(
                answered.pointer("/received/said").and_then(Value::as_str),
                Some("hello"),
                "case {row:?}: expected the backend's own answer where the selected response \
                 renders nothing, got {reply}"
            ),
            None => assert_eq!(
                answered.get("message").and_then(Value::as_str),
                Some(INTERNAL_MESSAGE),
                "case {row:?}: expected the gateway's own frame where the call selects no \
                 integration response, got {reply}"
            ),
        }
    }
}
