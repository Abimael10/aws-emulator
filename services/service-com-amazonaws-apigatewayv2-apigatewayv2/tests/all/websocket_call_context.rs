// SPDX-License-Identifier: CC0-1.0

//! What a WEBSOCKET route's integration is given about the call it serves, over a real
//! connection: the `$context` a MOCK integration's mapping templates resolve, and the
//! `@connections` management API the function an `AWS_PROXY` route fronts reaches through the
//! SDK its runtime supplies.
//!
//! The rule every table below is a slice of: **one call of a WebSocket connection carries ONE
//! context — what the gateway reports about that call — and every reader of a call reads the
//! same one: a mapping template resolves `$context.<member>` for each member the call reports
//! (and `$stageVariables.<name>` for the serving stage's own variables), and a function the
//! route fronts is handed that context as its event's `requestContext` and can call the
//! `@connections` API named in it, so a payload it posts to a connection id is delivered to
//! THAT connection.**
//!
//! The table of CONCURRENT sockets drives that rule over a SET of connections a deployed
//! stage carries at the same time, all of them served by the CATCH-ALL route: **every client
//! the announced endpoint accepts is minted its own connection id, so concurrent sockets of
//! one stage never share one, and a payload the serving route's function posts to an id is
//! delivered to that connection's socket whether or not it is the connection whose frame
//! triggered the call** — which is what makes a server-side broadcast to connections that sent
//! nothing observable at all.
//!
//! Nothing here is a value this emulator fills in for the test: each assertion is on a value
//! the gateway computed for the call (the route that served it, the stage, the api, the
//! address the client dialled) or on bytes a RUNNING function pushed over the socket, which
//! is why AWS Lambda and the `@connections` management API are mounted beside this service on
//! one engine.
//!
//! The FINAL table is what a backend learns when one of the connections it is pushing to has
//! DEPARTED: **every `@connections` call a route's function makes is answered with a complete
//! modelled result — the payload delivered and `200` for a connection the endpoint still
//! holds, the memberless `GoneException` (`410`, so no message at all) for an id of the
//! issued shape it no longer holds — so the function reads each outcome as that call's own
//! modelled answer, branches on it (pruning its record of a connection for the gone class and
//! for nothing else), goes on serving the rest of the call, and the frames it then pushes to
//! the connections it KEPT are delivered to them.** Never a transport failure and never a call
//! that does not come back: either leaves a handler with nothing to branch on, and the live
//! clients it had just been told about unnotified.
//!
//! The suites that own the steps before the integration are not restated here: the
//! handshake's own statuses are `websocket_transport_data_plane.rs`, the event each call hands
//! a function is `websocket_lambda_routes.rs`, and the `@connections` calls a backend makes
//! over the api's own socket are `websocket_endpoint_connections.rs`.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use crate::http_api_backend_dispatch::given_function;
use crate::websocket_endpoint_connections::minted;
use crate::websocket_transport_data_plane::{handshake_at, ClientSocket};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The stage every api below is deployed to.
const STAGE: &str = "prod";

/// The variables that stage carries, which `$stageVariables` names.
const STAGE_VARIABLE: (&str, &str) = ("tenant", "acme");

/// The source address a connection on the core's loopback listener is attributed to.
const LOOPBACK_SOURCE_IP: &str = "127.0.0.1";

/// How long a case waits for one frame before failing: generous for a loaded test machine —
/// a frame may be the answer of a function this emulator has to run first — and bounded so a
/// case that will never be answered fails instead of hanging the suite.
const FRAME_DEADLINE: Duration = Duration::from_secs(30);

/// How long a case waits for the FIRST frame a function owes it: that call starts the
/// runtime of the function the route fronts, which on a host running every other suite of the
/// workspace beside this one takes longer than a frame of an already-started one. A span no
/// case here tests, so it is given generous room; every later frame of the same case is
/// awaited for [`FRAME_DEADLINE`], so a case that stops being answered fails promptly. What
/// is waited for is the FRAME rather than the clock.
const RELAY_DEADLINE: Duration = Duration::from_secs(60);

/// How often a case looks again for a state the call it already made owes — the record of a
/// connection the client closed.
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The status each `@connections` operation answers for a connection that no longer exists
/// (`GoneException`, `@httpError(410)`).
const GONE: u16 = 410;

/// This service, the `@connections` management API and AWS Lambda on ONE engine: a
/// connection this api's socket carries is the connection that API addresses, and a route's
/// function is run by that Lambda.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One request against `host`, signed for `service`: its status and parsed body.
async fn signed(
    emulator: &MultiServiceRouter,
    host: &str,
    service: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("the body serializes")),
        None => bytes::Bytes::new(),
    };
    let request = smithy_server_runtime::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization(service))
        .body(payload)
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// What one `@connections` operation on `connection_id` answered, addressed at the
/// documented `…/{stage}/@connections/{id}` path with the id encoded the way an SDK client
/// encodes a path label.
async fn connections_call(
    emulator: &MultiServiceRouter,
    method: &str,
    connection_id: &str,
) -> (u16, Value) {
    let addressed = crate::path_label::addressed(connection_id);
    signed(
        emulator,
        &format!("execute-api.{REGION}.amazonaws.com"),
        "execute-api",
        method,
        &format!("/{STAGE}/@connections/{addressed}"),
        None,
    )
    .await
}

/// One control-plane request against THIS service.
async fn control(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    signed(
        emulator,
        &format!("apigateway.{REGION}.amazonaws.com"),
        "apigateway",
        method,
        uri,
        body,
    )
    .await
}

/// One control-plane request that must answer `201`.
async fn created(
    emulator: &MultiServiceRouter,
    case: &str,
    what: &str,
    uri: &str,
    body: Value,
) -> Value {
    let (status, answered) = control(emulator, "POST", uri, Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {what} must answer 201, got {status} {answered}"
    );
    answered
}

/// The member `name` of `body`, as text.
fn text(case: &str, what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no {name}: {body}"))
        .to_string()
}

/// One api, created with the socket its `ApiEndpoint` announces.
struct Api {
    api_id: String,
    /// The `host:port` of the api's own socket, read off the announced `ApiEndpoint` so a
    /// case dials the address the service reported.
    authority: String,
}

/// Create a WEBSOCKET api whose frames select a route by their `action` member.
async fn given_api(emulator: &MultiServiceRouter, case: &str, name: &str) -> Api {
    let created = created(
        emulator,
        case,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
    )
    .await;
    let announced = text(case, "CreateApi", &created, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
        })
        .to_string();
    Api {
        api_id: text(case, "CreateApi", &created, "apiId"),
        authority,
    }
}

/// Route `route_key` of `api_id` to a `MOCK` integration whose selected response renders
/// `template`, so what the client reads back IS that template rendered against the call.
async fn given_mock_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    template: &str,
) {
    let integration = created(
        emulator,
        case,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    let integration_id = text(case, "CreateIntegration", &integration, "integrationId");
    created(
        emulator,
        case,
        "CreateIntegrationResponse",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses"),
        json!({
            "integrationResponseKey": "$default",
            "responseTemplates": { "$default": template }
        }),
    )
    .await;
    given_route(emulator, case, api_id, route_key, &integration_id, true).await;
}

/// Route `route_key` of `api_id` to the function `function_arn` through an `AWS_PROXY`
/// integration, two-way when `two_way`.
async fn given_lambda_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    function_arn: &str,
    two_way: bool,
) {
    let integration = created(
        emulator,
        case,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_arn,
            "integrationMethod": "POST"
        }),
    )
    .await;
    let integration_id = text(case, "CreateIntegration", &integration, "integrationId");
    given_route(emulator, case, api_id, route_key, &integration_id, two_way).await;
}

/// Create the route and, when `two_way`, the `RouteResponse` that makes it two-way — "to
/// define a two-way route, create a route response".
async fn given_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    integration_id: &str,
    two_way: bool,
) {
    let route = created(
        emulator,
        case,
        "CreateRoute",
        &format!("/v2/apis/{api_id}/routes"),
        json!({
            "routeKey": route_key,
            "target": format!("integrations/{integration_id}")
        }),
    )
    .await;
    if two_way {
        let route_id = text(case, "CreateRoute", &route, "routeId");
        created(
            emulator,
            case,
            "CreateRouteResponse",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            json!({ "routeResponseKey": "$default" }),
        )
        .await;
    }
}

/// Create the stage every case dials, with the variables `$stageVariables` names, and
/// release a deployment to it.
async fn given_stage(emulator: &MultiServiceRouter, case: &str, api_id: &str) {
    let (name, value) = STAGE_VARIABLE;
    created(
        emulator,
        case,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": STAGE, "stageVariables": { name: value } }),
    )
    .await;
    created(
        emulator,
        case,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
    )
    .await;
}

/// Send one text frame.
async fn send(case: &str, socket: &mut ClientSocket, message: &str) {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the frame must be sent, got {error}"));
}

/// The next frame this socket is sent, as its text; a socket that is sent none within
/// [`FRAME_DEADLINE`] answers `None`.
async fn next_frame(case: &str, socket: &mut ClientSocket) -> Option<String> {
    next_frame_within(case, socket, FRAME_DEADLINE).await
}

/// [`next_frame`] awaited for as long as `within`: a frame a case must be sent is waited for
/// generously, because the call that produces it may have to start a function's runtime
/// first — a span no case here tests. The wait is for the FRAME, a condition, and nothing
/// asserts on the clock.
async fn next_frame_within(
    case: &str,
    socket: &mut ClientSocket,
    within: Duration,
) -> Option<String> {
    match tokio::time::timeout(within, socket.next()).await {
        Ok(Some(Ok(Message::Text(frame)))) => Some(frame.as_str().to_string()),
        Ok(Some(Ok(Message::Binary(frame)))) => Some(String::from_utf8_lossy(&frame).to_string()),
        Ok(Some(Ok(other))) => panic!("case {case:?}: unexpected frame {other:?}"),
        Ok(Some(Err(error))) => panic!("case {case:?}: the socket failed: {error}"),
        Ok(None) => panic!("case {case:?}: the socket closed instead of answering"),
        Err(_elapsed) => None,
    }
}

/// Send one frame and read the frame it is answered with.
async fn exchanged(case: &str, socket: &mut ClientSocket, message: &str) -> String {
    send(case, socket, message).await;
    next_frame(case, socket)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: {message} must be answered on the same socket"))
}

/// What a rendered value must be: the one text the call's own records give it, or the shape
/// of a value this run mints.
#[derive(Debug)]
enum Expect {
    /// Exactly this text, which the api's records decide.
    Exact(String),
    /// A non-empty value this call minted (an id of the call itself).
    NonEmpty,
    /// A number, which is what a template renders an instant of the call as.
    Digits,
}

/// Every `$context` member a WebSocket call reports resolves in the MOCK integration's
/// mapping template that reads it, with the value the call itself has.
#[tokio::test]
async fn a_mock_routes_template_resolves_the_context_of_the_call_it_serves() {
    let case = "a frame answered by a MOCK integration's response template";
    let (emulator, _engine) = emulator();
    let api = given_api(&emulator, case, "socket-call-context").await;

    // One route per reference under test, each fronting a MOCK integration whose selected
    // response is that reference alone — so the frame the client reads back IS the rendered
    // value. The rows span what the rule generalises over: each member the gateway reports
    // about a message call, a NESTED member (reached through the dotted path its nesting
    // gives it), the serving stage's own variables, and a template mixing the context with
    // the frame's payload. `routeKey` differs per row by construction: it is the route that
    // served the call.
    let rows: Vec<(&str, String, Expect)> = vec![
        (
            "the connection the frame arrived on",
            "$context.connectionId".to_string(),
            Expect::NonEmpty,
        ),
        (
            "the route that served the call",
            "$context.routeKey".to_string(),
            Expect::Exact("ctx-route-key".to_string()),
        ),
        (
            "the kind of call",
            "$context.eventType".to_string(),
            Expect::Exact("MESSAGE".to_string()),
        ),
        (
            "its direction",
            "$context.messageDirection".to_string(),
            Expect::Exact("IN".to_string()),
        ),
        (
            "the serving stage",
            "$context.stage".to_string(),
            Expect::Exact(STAGE.to_string()),
        ),
        (
            "the api",
            "$context.apiId".to_string(),
            Expect::Exact(api.api_id.clone()),
        ),
        (
            "the address the client dialled",
            "$context.domainName".to_string(),
            Expect::Exact(api.authority.clone()),
        ),
        (
            "a nested member of the context",
            "$context.identity.sourceIp".to_string(),
            Expect::Exact(LOOPBACK_SOURCE_IP.to_string()),
        ),
        (
            "the id of this call",
            "$context.requestId".to_string(),
            Expect::NonEmpty,
        ),
        (
            "when the connection was made",
            "$context.connectedAt".to_string(),
            Expect::Digits,
        ),
        (
            "a variable of the serving stage",
            format!("$stageVariables.{}", STAGE_VARIABLE.0),
            Expect::Exact(STAGE_VARIABLE.1.to_string()),
        ),
        (
            "the context beside the frame's own payload",
            r#"{"route":"$context.routeKey","sent":$input.json('$.action')}"#.to_string(),
            Expect::Exact(
                r#"{"route":"ctx-with-payload","sent":"ctx-with-payload"}"#.to_string(),
            ),
        ),
    ];
    // The route key each row's frame selects: the row's own name for the members whose
    // value does not depend on it, and a spelled-out key for the two rows that assert on
    // the route itself.
    let route_keys: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(at, (_what, template, _expected))| {
            if template.contains("routeKey") && template.starts_with('{') {
                "ctx-with-payload".to_string()
            } else if template.contains("routeKey") {
                "ctx-route-key".to_string()
            } else {
                format!("ctx-member-{at}")
            }
        })
        .collect();

    for ((_what, template, _expected), route_key) in rows.iter().zip(&route_keys) {
        given_mock_route(&emulator, case, &api.api_id, route_key, template).await;
    }
    given_stage(&emulator, case, &api.api_id).await;

    let mut socket: ClientSocket = handshake_at(case, &api.authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: the handshake must be completed, got {status}")
        });

    let mut rendered_connection_id = String::new();
    for ((what, template, expected), route_key) in rows.iter().zip(&route_keys) {
        let frame = exchanged(case, &mut socket, &json!({ "action": route_key }).to_string()).await;
        match expected {
            Expect::Exact(value) => assert_eq!(
                &frame, value,
                "case {case:?}: the template {template:?} must render {what} as {value:?}, got \
                 {frame:?}"
            ),
            Expect::NonEmpty => assert!(
                !frame.trim().is_empty(),
                "case {case:?}: the template {template:?} must render {what}, got {frame:?}"
            ),
            Expect::Digits => assert!(
                !frame.is_empty() && frame.chars().all(|glyph| glyph.is_ascii_digit()),
                "case {case:?}: the template {template:?} must render {what} as a number, got \
                 {frame:?}"
            ),
        }
        if template == "$context.connectionId" {
            rendered_connection_id = frame;
        }
    }

    // The id the template rendered is the connection's own, not a value this emulator made
    // up for the template: the `@connections` management API reports that connection.
    let (status, reported) = connections_call(&emulator, "GET", &rendered_connection_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetConnection on the rendered id {rendered_connection_id:?} must answer \
         200, got {status} {reported}"
    );
    assert!(
        reported.get("connectedAt").is_some(),
        "case {case:?}: the reported connection must carry connectedAt, got {reported}"
    );
}

/// The CLOSING handshake and what it leaves behind: a client's close frame is answered with
/// a close frame, and the id that connection was minted then addresses a connection that no
/// longer exists.
#[tokio::test]
async fn a_clients_close_frame_is_answered_with_a_close_frame() {
    let case = "a client closing a carried connection";
    let (emulator, _engine) = emulator();
    let api = given_api(&emulator, case, "socket-closing-handshake").await;
    // One route, so a socket can learn the id its connection was minted before it closes.
    given_mock_route(
        &emulator,
        case,
        &api.api_id,
        "whoami",
        "$context.connectionId",
    )
    .await;
    given_stage(&emulator, case, &api.api_id).await;

    // The rows span what the closing handshake generalises over: the normal closure a client
    // library sends, another status with a reason phrase, and a close frame carrying no
    // status at all (RFC 6455 §7.4.1's "no status code was actually present").
    let rows: [(&str, Option<CloseFrame>); 3] = [
        (
            "the normal closure",
            Some(CloseFrame {
                code: CloseCode::Normal,
                reason: String::new().into(),
            }),
        ),
        (
            "a status with a reason",
            Some(CloseFrame {
                code: CloseCode::Away,
                reason: "going away".to_string().into(),
            }),
        ),
        ("no status at all", None),
    ];
    for (what, close) in rows {
        let mut socket: ClientSocket = handshake_at(case, &api.authority, &format!("/{STAGE}"))
            .await
            .unwrap_or_else(|status| {
                panic!("case {case:?}: the handshake must be completed, got {status}")
            });
        let connection_id = exchanged(case, &mut socket, r#"{"action":"whoami"}"#).await;
        socket
            .send(Message::Close(close))
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: {what} must be sent, got {error}")
            });
        match tokio::time::timeout(FRAME_DEADLINE, socket.next()).await {
            Ok(Some(Ok(Message::Close(_echo)))) => {}
            other => panic!(
                "case {what:?}: the close must be answered with a close frame, got {other:?}"
            ),
        }
        // The connection's record ends with it, which the `@connections` API reports: the
        // close is a call of the connection (the `$disconnect` route's), so the record goes
        // once that call is served rather than the instant the frame arrived.
        let deadline = std::time::Instant::now() + FRAME_DEADLINE;
        let mut last = (0u16, Value::Null);
        while std::time::Instant::now() < deadline {
            last = connections_call(&emulator, "GET", &connection_id).await;
            if last.0 == GONE {
                break;
            }
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
        assert_eq!(
            last.0, GONE,
            "case {what:?}: the closed connection {connection_id:?} must answer {GONE}, got {} {}",
            last.0, last.1
        );
    }
}

/// every connection a frame names through the `@connections` API of the endpoint its event
/// reports.
fn pushing_handler() -> String {
    "import json\n\
     import boto3\n\
     \n\
     \n\
     def lambda_handler(event, context):\n\
     \x20   call = json.loads(event.get(\"body\") or \"{}\")\n\
     \x20   reported = event[\"requestContext\"]\n\
     \x20   if call.get(\"action\") == \"whoami\":\n\
     \x20       return {\"statusCode\": 200,\n\
     \x20               \"body\": json.dumps({\"connectionId\": reported[\"connectionId\"],\n\
     \x20                                   \"routeKey\": reported[\"routeKey\"]})}\n\
     \x20   connections = boto3.client(\n\
     \x20       \"apigatewaymanagementapi\",\n\
     \x20       endpoint_url=\"https://%s/%s\" % (reported[\"domainName\"], reported[\"stage\"]),\n\
     \x20   )\n\
     \x20   if call.get(\"action\") == \"inspect\":\n\
     \x20       known = connections.get_connection(ConnectionId=call[\"target\"])\n\
     \x20       connections.post_to_connection(\n\
     \x20           ConnectionId=reported[\"connectionId\"],\n\
     \x20           Data=json.dumps({\"connectedAt\": str(known.get(\"ConnectedAt\"))}).encode(\n\
     \x20               \"utf-8\"\n\
     \x20           ),\n\
     \x20       )\n\
     \x20       return {\"statusCode\": 200}\n\
     \x20   for target in call.get(\"targets\") or []:\n\
     \x20       connections.post_to_connection(\n\
     \x20           ConnectionId=target, Data=call[\"message\"].encode(\"utf-8\")\n\
     \x20       )\n\
     \x20   return {\"statusCode\": 200}\n"
        .to_string()
}

/// The connection id a socket learns from the two-way `whoami` route, which is the id the
/// function was given for that connection.
async fn whoami(case: &str, socket: &mut ClientSocket) -> String {
    let frame = exchanged(case, socket, r#"{"action":"whoami"}"#).await;
    let reply: Value = serde_json::from_str(&frame).unwrap_or_else(|error| {
        panic!("case {case:?}: the whoami reply must be the function's JSON body, got {frame:?} ({error})")
    });
    assert_eq!(
        reply.get("routeKey").and_then(Value::as_str),
        Some("whoami"),
        "case {case:?}: the reply must come from the whoami route, got {frame:?}"
    );
    reply
        .get("connectionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| panic!("case {case:?}: the reply must carry a connectionId, got {frame:?}"))
        .to_string()
}

/// A payload a route's function posts through the `@connections` API is delivered to the
/// connection it named, and to no other.
#[tokio::test]
async fn a_routes_function_posts_to_the_connections_it_names() {
    let case = "a route's function fanning a payload out over @connections";
    let (emulator, engine) = emulator();
    let function = given_function(&emulator, &engine, case, "socket-push", &pushing_handler()).await;
    let api = given_api(&emulator, case, "socket-push-api").await;
    // `whoami` is TWO-WAY, so the function's own answer reaches the client that asked; the
    // pushing routes are one-way, so the only frames they produce are the ones the function
    // posted.
    for (route_key, two_way) in [("whoami", true), ("push", false), ("inspect", false)] {
        given_lambda_route(&emulator, case, &api.api_id, route_key, &function, two_way).await;
    }
    given_stage(&emulator, case, &api.api_id).await;

    let mut first: ClientSocket = handshake_at(case, &api.authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: the first handshake must be completed, got {status}")
        });
    let mut second: ClientSocket = handshake_at(case, &api.authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: the second handshake must be completed, got {status}")
        });
    let first_id = whoami(case, &mut first).await;
    let second_id = whoami(case, &mut second).await;
    assert_ne!(
        first_id, second_id,
        "case {case:?}: each connection must be minted its own id, got {first_id:?} twice"
    );

    // The rows span what the fan-out generalises over: a payload posted to the connection
    // that sent the frame, one posted to the OTHER connection, and one posted to both. Each
    // row carries its own marker and each socket is read in row order, so a payload
    // delivered to a connection it was not posted to is read as the wrong marker by the
    // next row that reads that socket.
    let rows: [(&str, Vec<&String>, &str); 3] = [
        ("the sender's own connection", vec![&first_id], "first-only"),
        ("another connection", vec![&second_id], "second-only"),
        ("both connections", vec![&first_id, &second_id], "both"),
    ];
    for (what, targets, marker) in rows {
        let message = json!({
            "action": "push",
            "targets": targets,
            "message": marker,
        })
        .to_string();
        let expects_first = targets.contains(&&first_id);
        send(case, &mut first, &message).await;
        if expects_first {
            let delivered = next_frame(case, &mut first)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: {what} must be delivered to the sender"));
            assert_eq!(
                delivered, marker,
                "case {case:?}: {what} must be delivered the payload {marker:?}, got \
                 {delivered:?}"
            );
        }
        if targets.contains(&&second_id) {
            let delivered = next_frame(case, &mut second).await.unwrap_or_else(|| {
                panic!("case {case:?}: {what} must be delivered to the connection it named")
            });
            assert_eq!(
                delivered, marker,
                "case {case:?}: {what} must be delivered the payload {marker:?}, got \
                 {delivered:?}"
            );
        }
    }

    // The other operations of the same API are reachable the same way: the function reads
    // the connection it was asked about and posts what that read reported.
    send(
        case,
        &mut first,
        &json!({ "action": "inspect", "target": second_id }).to_string(),
    )
    .await;
    let reported = next_frame(case, &mut first)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the inspected connection must be reported back"));
    let reported: Value = serde_json::from_str(&reported).unwrap_or_else(|error| {
        panic!("case {case:?}: the pushed payload must be the function's JSON, got {reported:?} ({error})")
    });
    assert!(
        reported
            .get("connectedAt")
            .and_then(Value::as_str)
            .is_some_and(|instant| !instant.trim().is_empty() && instant != "None"),
        "case {case:?}: GetConnection must report when the connection was made, got {reported}"
    );
}

/// The CATCH-ALL route key of a WebSocket api: the route that serves every message whose
/// selected value names no other route of the api, and so the only route the table below
/// creates.
const CATCH_ALL_ROUTE_KEY: &str = "$default";

/// How many clients the table below opens on one deployed stage at the same time: three, so
/// "the sender", "another connection" and "every connection but the sender" are three
/// different sets and a payload delivered to the wrong socket cannot pass for a delivery to
/// the right one.
const CONCURRENT_SOCKETS: usize = 3;

/// The `DeploymentStatus` a released deployment carries, of the values `DeploymentStatus`
/// declares (`PENDING | FAILED | DEPLOYED`): the state a client waits for before it dials the
/// stage.
const DEPLOYED: &str = "DEPLOYED";

/// A payload as an assertion names it: short text in full, and long text by its first
/// characters and its byte count, so a failure stays readable whatever the row posted.
fn named(payload: &str) -> String {
    let shown: String = payload.chars().take(48).collect();
    if shown.len() == payload.len() {
        format!("{shown:?}")
    } else {
        format!("{shown:?}… ({} bytes in all)", payload.len())
    }
}

/// The id the connection `what` carries was minted, learned from the function the CATCH-ALL
/// route fronts.
///
/// The reply names the route that served the call, so a message whose `action` matches no
/// route key of the api is answered by `$default` — and the id it carries is the one the
/// gateway reported to the function for THIS connection.
async fn connection_of(case: &str, what: &str, socket: &mut ClientSocket) -> String {
    let frame = exchanged(case, socket, r#"{"action":"whoami"}"#).await;
    let reply: Value = serde_json::from_str(&frame).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {what}'s reply must be the function's JSON body, got {frame:?} \
             ({error})"
        )
    });
    assert_eq!(
        reply.get("routeKey").and_then(Value::as_str),
        Some(CATCH_ALL_ROUTE_KEY),
        "case {case:?}: {what}'s message must be served by the catch-all route, got {frame:?}"
    );
    reply
        .get("connectionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| {
            panic!("case {case:?}: {what}'s reply must carry a connectionId, got {frame:?}")
        })
        .to_string()
}

/// Several clients of ONE deployed stage, each minted its own connection id, and a payload the
/// serving route's function posts to an id delivered to that connection's socket — the
/// sender's own and, which is what a broadcast is made of, the sockets that sent nothing.
#[tokio::test]
async fn every_socket_of_a_deployed_stage_is_posted_to_by_the_id_it_was_minted() {
    /// One broadcast: which socket sends it, which connections the frame names, and the bytes
    /// the function posts to each of them.
    struct Row {
        what: &'static str,
        sender: usize,
        targets: Vec<usize>,
        message: String,
    }

    let case = "a broadcast over the catch-all route of a deployed stage";
    let (emulator, engine) = emulator();
    let function =
        given_function(&emulator, &engine, case, "socket-broadcast", &pushing_handler()).await;
    let api = given_api(&emulator, case, "socket-broadcast-api").await;
    // ONE route, the catch-all, made two-way by its route response: every message of every
    // connection is served by it, whatever that message's `action` names.
    given_lambda_route(
        &emulator,
        case,
        &api.api_id,
        CATCH_ALL_ROUTE_KEY,
        &function,
        true,
    )
    .await;
    given_stage(&emulator, case, &api.api_id).await;

    // The stage is dialled only once the api reports the deployment released to it as the
    // status a client waits for.
    let (status, deployments) = control(
        &emulator,
        "GET",
        &format!("/v2/apis/{}/deployments", api.api_id),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetDeployments must answer 200, got {status} {deployments}"
    );
    let released = deployments["items"].as_array().cloned().unwrap_or_default();
    assert!(
        !released.is_empty(),
        "case {case:?}: the stage must carry the deployment that was released to it, got \
         {deployments}"
    );
    for deployment in &released {
        assert_eq!(
            deployment.get("deploymentStatus").and_then(Value::as_str),
            Some(DEPLOYED),
            "case {case:?}: the deployment the stage serves must be {DEPLOYED}, got {deployment}"
        );
    }

    // Every client dials the address the api announced, independently of the others.
    let mut sockets: Vec<ClientSocket> = Vec::with_capacity(CONCURRENT_SOCKETS);
    for at in 0..CONCURRENT_SOCKETS {
        sockets.push(
            handshake_at(case, &api.authority, &format!("/{STAGE}"))
                .await
                .unwrap_or_else(|status| {
                    panic!("case {case:?}: socket {at}'s handshake must be completed, got {status}")
                }),
        );
    }

    // Each socket carries a connection of its OWN: the id it learns is the one the gateway
    // minted for it, so no two concurrent sockets of this stage share one.
    let mut ids: Vec<String> = Vec::with_capacity(CONCURRENT_SOCKETS);
    for (at, socket) in sockets.iter_mut().enumerate() {
        let what = format!("socket {at}");
        ids.push(connection_of(case, &what, socket).await);
    }
    for (left, id) in ids.iter().enumerate() {
        for (right, other) in ids.iter().enumerate().skip(left + 1) {
            assert_ne!(
                id, other,
                "case {case:?}: sockets {left} and {right} must each be minted their own \
                 ConnectionId, got {id:?} twice"
            );
        }
        // And the id names a connection the management API HOLDS, so what a broadcast
        // addresses is a record of this stage rather than a value the reply made up.
        let (status, reported) = connections_call(&emulator, "GET", id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetConnection on socket {left}'s id {id:?} must answer 200, got \
             {status} {reported}"
        );
    }

    // The rows span what the fan-out generalises over: WHICH connections one call names
    // relative to the connection it serves (the sender alone, one that sent nothing, every
    // connection but the sender, all of them), WHICH socket sends the call, HOW MANY
    // connections it names (one, two, three), and the FORM of the payload (ASCII, characters
    // of several bytes, text carrying the delimiters of the envelope it travelled in, and a
    // payload of many bytes). Every row carries its own payload and each socket is read in row
    // order, so a payload delivered to a connection the call did not name is read as the wrong
    // payload by the next row that reads that socket — and the last row names every socket, so
    // none is left unread.
    let rows = vec![
        Row {
            what: "the connection that sent the frame",
            sender: 0,
            targets: vec![0],
            message: "to the sender alone".to_string(),
        },
        Row {
            what: "one connection that sent nothing",
            sender: 0,
            targets: vec![1],
            message: "to a connection that sent nothing".to_string(),
        },
        Row {
            what: "every connection but the sender",
            sender: 2,
            targets: vec![0, 1],
            message: "héllo ✓ to the others".to_string(),
        },
        Row {
            what: "every connection of the stage, from another sender",
            sender: 1,
            targets: vec![0, 1, 2],
            message: "{\"quoted\":\"value\"}\nand a second line".to_string(),
        },
        Row {
            what: "a payload of many bytes, to every connection",
            sender: 0,
            targets: vec![0, 1, 2],
            message: "payload-".repeat(2 * 1024),
        },
    ];

    for row in &rows {
        let what = row.what;
        let targets: Vec<&str> = row.targets.iter().map(|at| ids[*at].as_str()).collect();
        let frame = json!({
            "action": "broadcast",
            "targets": targets,
            "message": row.message,
        })
        .to_string();
        send(case, &mut sockets[row.sender], &frame).await;
        for at in &row.targets {
            let delivered = next_frame(case, &mut sockets[*at]).await.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: {what} must be delivered to socket {at}, which the call named"
                )
            });
            assert!(
                delivered == row.message,
                "case {case:?}: {what} must deliver exactly {} to socket {at}, got {}",
                named(&row.message),
                named(&delivered)
            );
        }
    }

    // The closing handshake, over a stage carrying several live connections: each client's
    // close frame is answered with a close frame of the server's own, one socket at a time.
    for (at, socket) in sockets.iter_mut().enumerate() {
        socket
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: String::new().into(),
            })))
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: socket {at}'s close must be sent, got {error}")
            });
        match tokio::time::timeout(FRAME_DEADLINE, socket.next()).await {
            Ok(Some(Ok(Message::Close(_echo)))) => {}
            other => panic!(
                "case {case:?}: socket {at}'s close must be answered with a close frame, got \
                 {other:?}"
            ),
        }
    }

    // And the registry ends with those connections: every id this stage minted names none.
    // The close is a call of the connection, so the record goes once that call is served
    // rather than the instant the frame arrived — the case looks again until it has.
    for (at, id) in ids.iter().enumerate() {
        let deadline = std::time::Instant::now() + FRAME_DEADLINE;
        let mut last = (0u16, Value::Null);
        while std::time::Instant::now() < deadline {
            last = connections_call(&emulator, "GET", id).await;
            if last.0 == GONE {
                break;
            }
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
        assert_eq!(
            last.0, GONE,
            "case {case:?}: socket {at}'s closed connection {id:?} must answer {GONE}, got {} {}",
            last.0, last.1
        );
    }
}

/// The route key that registers a connection in the backend's own table, and the one that
/// fans a payload out over it. The api selects a route by the frame's `action`
/// ([`given_api`]), so these are the values a frame names.
const REGISTER_ROUTE_KEY: &str = "register";
const FAN_ROUTE_KEY: &str = "fan";

/// The node labels the registering clients name themselves with, one per connection: three
/// forms a client spells a node with, so a reply or a delivery that reached the wrong socket
/// is read as another node's.
const NODE_LABELS: [&str; 3] = ["ui-west-1", "node.42", "LONGER-NODE-LABEL-0000000001"];

/// The node the backend's table knows the id this stage NEVER held by — a row of its table
/// that could only ever be answered the gone class.
const UNHELD_NODE: &str = "node-from-another-process";

/// The error shape a push to a connection the endpoint no longer holds is refused with
/// (`GoneException`, `@httpError(410)`, declared with NO members, so it carries no message at
/// all), and the status a delivered push answers.
const GONE_CODE: &str = "GoneException";
const DELIVERED_STATUS: u16 = 200;

/// Which socket sends every fan-out below, and so the connection the backend notifies whatever
/// the call named.
const SENDER: usize = 0;

/// A function that keeps the connection table AWS's own guidance has a backend keep, in the
/// SDK its runtime supplies: it answers the frame that registers a connection, and for every
/// other call it pushes the payload to each id it was handed, records what its SDK was
/// ANSWERED for that id, prunes its row only where the connection has GONE, and notifies the
/// connections it kept — the sender's included.
///
/// Everything it reports is something its own SDK was answered, so a push that produced no
/// modelled result at all (a severed connection, a call that never came back) leaves the
/// notice unsent and the rows below unread.
const PRUNING_HANDLER: &str = r#"import json
import boto3
import botocore.exceptions

GONE = "GoneException"


def lambda_handler(event, context):
    call = json.loads(event.get("body") or "{}")
    reported = event["requestContext"]
    if call.get("action") == "register":
        # The reply a two-way route relays to the socket that sent the frame: the node this
        # connection registered as, and the id the gateway reported for it.
        return {
            "statusCode": 200,
            "body": json.dumps(
                {"node": call["node"], "connectionId": reported["connectionId"]}
            ),
        }
    connections = boto3.client(
        "apigatewaymanagementapi",
        endpoint_url="https://%s/%s" % (reported["domainName"], reported["stage"]),
    )
    kept = []
    pruned = []
    observed = []
    for target in call.get("targets") or []:
        try:
            answered = connections.post_to_connection(
                ConnectionId=target["connectionId"],
                Data=json.dumps(
                    {"node": target["node"], "message": call["message"]}
                ).encode("utf-8"),
            )
        except botocore.exceptions.ClientError as error:
            refusal = error.response["Error"]
            observed.append(
                {
                    "node": target["node"],
                    "code": refusal["Code"],
                    "message": refusal.get("Message") or "",
                    "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
                }
            )
            # A row is dropped for a connection that has GONE, and for nothing else.
            if refusal["Code"] == GONE:
                pruned.append(target["node"])
            else:
                kept.append(target)
            continue
        observed.append(
            {
                "node": target["node"],
                "code": "",
                "message": "",
                "status": answered["ResponseMetadata"]["HTTPStatusCode"],
            }
        )
        kept.append(target)
    notice = json.dumps({"notice": "pruned", "pruned": pruned, "observed": observed})
    notified = [reported["connectionId"]]
    for target in kept:
        if target["connectionId"] not in notified:
            notified.append(target["connectionId"])
    for connection_id in notified:
        connections.post_to_connection(
            ConnectionId=connection_id, Data=notice.encode("utf-8")
        )
    return {"statusCode": 200, "body": notice}
"#;

/// One row of the backend's connection table: the node it knows the connection by, the id it
/// addresses, and whether the endpoint still holds that connection.
struct Registered {
    node: &'static str,
    connection_id: String,
    /// A connection the endpoint HOLDS, whose push is delivered; a row that is not live is one
    /// the endpoint reports gone.
    live: bool,
}

/// Register `node` over this socket through the two-way [`REGISTER_ROUTE_KEY`] route, and
/// answer the connection id the function was given for it — the reply the gateway relayed back
/// over the socket that sent the frame.
async fn registered(case: &str, node: &str, socket: &mut ClientSocket) -> String {
    let frame = json!({ "action": REGISTER_ROUTE_KEY, "node": node }).to_string();
    send(case, socket, &frame).await;
    let what = format!("the registration reply for {node:?}");
    let reply = next_frame_within(case, socket, RELAY_DEADLINE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: {what} must be relayed over its own socket"));
    let parsed: Value = serde_json::from_str(&reply).unwrap_or_else(|error| {
        panic!("case {case:?}: {what} must be the function's JSON body, got {reply:?} ({error})")
    });
    assert_eq!(
        parsed.get("node").and_then(Value::as_str),
        Some(node),
        "case {case:?}: {what} must carry the node this connection registered as, got {reply:?}"
    );
    parsed
        .get("connectionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| panic!("case {case:?}: {what} must carry a connectionId, got {reply:?}"))
        .to_string()
}

/// The frame `what` as the JSON the function pushed, or a panic naming the case.
fn pushed_json(case: &str, what: &str, frame: &str) -> Value {
    serde_json::from_str(frame).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {what} must be the JSON the function pushed, got {frame:?} ({error})"
        )
    })
}

/// A payload a route's function pushes to a DEPARTED connection is the modelled
/// `GoneException` its SDK reads, so the function prunes that row alone and the connections it
/// kept are notified over their own sockets.
#[tokio::test]
async fn a_routes_function_prunes_only_the_connection_the_endpoint_reports_gone() {
    let case = "a route's function fanning out over a table holding a departed connection";
    let (emulator, engine) = emulator();
    let function = given_function(&emulator, &engine, case, "socket-prune", PRUNING_HANDLER).await;
    let api = given_api(&emulator, case, "socket-prune-api").await;
    // The registration route is TWO-WAY, so its answer reaches the socket that registered;
    // the fan-out route is one-way, so the only frames it produces are the ones the function
    // pushed.
    given_lambda_route(
        &emulator,
        case,
        &api.api_id,
        REGISTER_ROUTE_KEY,
        &function,
        true,
    )
    .await;
    given_lambda_route(&emulator, case, &api.api_id, FAN_ROUTE_KEY, &function, false).await;
    given_stage(&emulator, case, &api.api_id).await;

    // Three clients dial the announced socket and register, each with its own node label: the
    // reply each reads is the one its OWN frame earned, and the id it carries names a
    // connection the management API holds.
    let mut sockets: Vec<ClientSocket> = Vec::with_capacity(NODE_LABELS.len());
    let mut ids: Vec<String> = Vec::with_capacity(NODE_LABELS.len());
    for node in NODE_LABELS {
        let mut socket: ClientSocket = handshake_at(case, &api.authority, &format!("/{STAGE}"))
            .await
            .unwrap_or_else(|status| {
                panic!("case {case:?}: the handshake for {node:?} must be completed, got {status}")
            });
        let connection_id = registered(case, node, &mut socket).await;
        let (status, reported) = connections_call(&emulator, "GET", &connection_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetConnection on {node:?}'s id {connection_id:?} must answer 200, \
             got {status} {reported}"
        );
        sockets.push(socket);
        ids.push(connection_id);
    }
    for (left, id) in ids.iter().enumerate() {
        for (right, other) in ids.iter().enumerate().skip(left + 1) {
            assert_ne!(
                id, other,
                "case {case:?}: the connections of {:?} and {:?} must each be minted their own \
                 id, got {id:?} twice",
                NODE_LABELS[left], NODE_LABELS[right]
            );
        }
    }

    // The LAST registered connection then departs — a `DeleteConnection` ends it definitely,
    // so no row below waits on a close — and the endpoint reports it gone from then on.
    let stale_node = NODE_LABELS[NODE_LABELS.len() - 1];
    let stale_id = ids[ids.len() - 1].clone();
    let (status, deleted) = connections_call(&emulator, "DELETE", &stale_id).await;
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteConnection on {stale_node:?} must answer 204, got {status} {deleted}"
    );
    drop(sockets.pop());
    let deadline = std::time::Instant::now() + FRAME_DEADLINE;
    let mut last = (0u16, Value::Null);
    while std::time::Instant::now() < deadline {
        last = connections_call(&emulator, "GET", &stale_id).await;
        if last.0 == GONE {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        last.0, GONE,
        "case {case:?}: the departed connection {stale_id:?} must answer {GONE}, got {} {}",
        last.0, last.1
    );

    // The table the fan-outs loop over: the two live connections, the one that departed, and
    // a row naming an id of the issued shape this stage never held at all — both of the ways a
    // backend's row can name a connection the endpoint does not hold.
    let table = vec![
        Registered {
            node: NODE_LABELS[0],
            connection_id: ids[0].clone(),
            live: true,
        },
        Registered {
            node: NODE_LABELS[1],
            connection_id: ids[1].clone(),
            live: true,
        },
        Registered {
            node: stale_node,
            connection_id: stale_id.clone(),
            live: false,
        },
        Registered {
            node: UNHELD_NODE,
            connection_id: minted("never-held-by-this-stage"),
            live: false,
        },
    ];
    // The first rows of the table are the live connections, in the order their sockets were
    // opened, which is what lets a row read the delivery owed to socket `at` off `sockets[at]`.
    for (at, row) in table.iter().enumerate().take(sockets.len()) {
        assert_eq!(
            row.connection_id, ids[at],
            "case {case:?}: table row {at} must be the connection socket {at} carries"
        );
    }

    // The rows span what the rule generalises over: how many of the addressed connections the
    // endpoint still holds (all, all but one, one, none), WHERE the departed ones sit in the
    // fan-out (last, in the middle, first), and whether the row the endpoint does not hold was
    // issued here and closed or never held at all. Each row carries its own marker, and each
    // socket is read in row order, so a frame delivered to a connection a row did not name is
    // read as the wrong marker by the next row that reads that socket — and the last row names
    // every live connection, so none is left unread.
    let rows: Vec<(&str, Vec<usize>, &str)> = vec![
        (
            "every addressed connection is one the endpoint holds",
            vec![0, 1],
            "both-live",
        ),
        (
            "a departed connection addressed between two live ones",
            vec![0, 2, 1],
            "one-departed",
        ),
        (
            "the departed connection addressed first, beside an id this stage never held",
            vec![2, 3, 0],
            "two-refused",
        ),
        (
            "only a connection the endpoint no longer holds is addressed",
            vec![2],
            "nothing-delivered",
        ),
        // The LAST fan-out names both live connections again: each reads one delivery and one
        // notice here, so a frame pushed to a connection an earlier row did not name — the row
        // that addressed nothing live included — is read in this row's place instead of going
        // unobserved.
        (
            "every addressed connection is live again, after every refusal",
            vec![0, 1],
            "both-live-last",
        ),
    ];

    for (what, addressed, marker) in &rows {
        let targets: Vec<Value> = addressed
            .iter()
            .map(|at| json!({ "connectionId": table[*at].connection_id, "node": table[*at].node }))
            .collect();
        let frame = json!({
            "action": FAN_ROUTE_KEY,
            "targets": targets,
            "message": marker,
        })
        .to_string();
        send(case, &mut sockets[SENDER], &frame).await;

        // What the fan-out owes: a delivery to each addressed connection the endpoint holds,
        // the gone class for every other addressed row, and the notice to the sender and to
        // each connection the function kept.
        let delivered: Vec<usize> = addressed
            .iter()
            .copied()
            .filter(|at| table[*at].live)
            .collect();
        let pruned: Vec<&str> = addressed
            .iter()
            .copied()
            .filter(|at| !table[*at].live)
            .map(|at| table[at].node)
            .collect();

        for at in 0..sockets.len() {
            let owed_delivery = delivered.contains(&at);
            let owed_notice = at == SENDER || owed_delivery;
            if owed_delivery {
                let label = format!("{what}: the payload addressed to {:?}", table[at].node);
                let read = next_frame_within(case, &mut sockets[at], FRAME_DEADLINE)
                    .await
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: {label} must be delivered over its own socket")
                    });
                let payload = pushed_json(case, &label, &read);
                assert_eq!(
                    payload.get("node").and_then(Value::as_str),
                    Some(table[at].node),
                    "case {case:?}: {label} must name the node it was addressed to, got {read:?}"
                );
                assert_eq!(
                    payload.get("message").and_then(Value::as_str),
                    Some(*marker),
                    "case {case:?}: {label} must carry this row's payload, got {read:?}"
                );
            }
            if !owed_notice {
                continue;
            }
            let label = format!("{what}: the notice {:?} is sent", table[at].node);
            let read = next_frame_within(case, &mut sockets[at], FRAME_DEADLINE)
                .await
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {label} must reach the connection it was pushed to")
                });
            let notice = pushed_json(case, &label, &read);
            let reported_pruned: Vec<&str> = notice
                .get("pruned")
                .and_then(Value::as_array)
                .map(|nodes| nodes.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                reported_pruned, pruned,
                "case {case:?}: {label} must prune exactly the connections the endpoint reports \
                 gone, got {read:?}"
            );
            // And what the function's own SDK was answered for each id it addressed, in the
            // order it addressed them: the modelled gone class for a connection the endpoint
            // no longer holds, and the delivered status for one it does.
            let observed = notice
                .get("observed")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                observed.len(),
                addressed.len(),
                "case {case:?}: {label} must report one answer per addressed id, got {read:?}"
            );
            for (seen, at_addressed) in observed.iter().zip(addressed.iter().copied()) {
                let row = &table[at_addressed];
                let expected_status = if row.live { DELIVERED_STATUS } else { GONE };
                let expected_code = if row.live { "" } else { GONE_CODE };
                assert_eq!(
                    seen.get("node").and_then(Value::as_str),
                    Some(row.node),
                    "case {case:?}: {label} must report the ids in the order they were \
                     addressed, got {read:?}"
                );
                assert_eq!(
                    seen.get("status").and_then(Value::as_u64),
                    Some(u64::from(expected_status)),
                    "case {case:?}: {label} must report {expected_status} for {:?}, got {read:?}",
                    row.node
                );
                assert_eq!(
                    seen.get("code").and_then(Value::as_str),
                    Some(expected_code),
                    "case {case:?}: {label} must report the error class {expected_code:?} for \
                     {:?}, got {read:?}",
                    row.node
                );
                // `GoneException` declares no members, so the refusal a departed connection
                // earns carries no sentence at all — what the SDK reports for it is the empty
                // message. A delivered push is no refusal and has none to report either, which
                // its status and empty error class above already say.
                if !row.live {
                    assert_eq!(
                        seen.get("message").and_then(Value::as_str),
                        Some(""),
                        "case {case:?}: {label} must carry no message for {:?} — GoneException \
                         is memberless — got {read:?}",
                        row.node
                    );
                }
            }
        }
    }

    // The fan-outs left the registry where they found it: every live connection is still
    // reported, and neither id the function pruned names one.
    for row in &table {
        let expected = if row.live { 200 } else { GONE };
        let (status, reported) = connections_call(&emulator, "GET", &row.connection_id).await;
        assert_eq!(
            status, expected,
            "case {case:?}: after the fan-outs GetConnection on {:?} must answer {expected}, got \
             {status} {reported}",
            row.node
        );
    }
}
