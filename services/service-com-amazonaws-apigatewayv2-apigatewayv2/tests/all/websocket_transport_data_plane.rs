// SPDX-License-Identifier: CC0-1.0

//! The WEBSOCKET api's own SOCKET, driven over a real connection: the RFC 6455 opening
//! handshake it completes (or refuses), the frames it dispatches, and the replies a client
//! reads back.
//!
//! The rule these cases assert, one level above the recorded cases (one api, one MOCK
//! integration, three integration responses, one stage):
//!
//! > A WEBSOCKET api announces, as its `ApiEndpoint`, the address of the socket it is served
//! > on, and that socket answers an RFC 6455 opening handshake FROM THE API'S OWN RECORDS:
//! > a stage the endpoint does not serve (no stage of that name, or no deployment released
//! > to it) is refused `403`; a stage whose `$connect` route's integration cannot reply
//! > successfully is refused `500`; any other deployed stage is answered `101 Switching
//! > Protocols`. Over the established socket each message is dispatched to the route the
//! > api's `RouteSelectionExpression` selects (or `$default`), that route's integration
//! > reply is selected among its `IntegrationResponses` by the status the call produced, and
//! > a TWO-WAY route's selected response renders the payload the client reads back. The
//! > connection the handshake minted is the connection the `@connections` management API
//! > addresses: a `PostToConnection` to it is DELIVERED over that socket, and once the
//! > client closes, each of the three `@connections` operations answers `GoneException`.
//!
//! The selection itself is the one slice stated separately, because it is a rule about the
//! EXPRESSION rather than about the socket:
//!
//! > The `RouteSelectionExpression` of a WEBSOCKET api names a PATH into each message —
//! > `$request.body.<member>[.<member>…]` — and the key a frame is routed by is the value
//! > that path reaches, resolved member by member from the top of the message; a path the
//! > frame carries no value for selects nothing, which is what `$default` serves. The api's
//! > CURRENT expression is the one a deployed stage selects by, so a frame sent after the
//! > expression was changed and the stage redeployed is routed by the new path.
//!
//! And the LIFETIME of the connection the handshake established, which the last two cases
//! below own:
//!
//! > A connection that completed the handshake LASTS: it carries message after message, each
//! > dispatched to the route the api's expression selects for it, and it stays the same
//! > connection throughout. Every frame the client reads over it is a payload the api
//! > PRODUCED — byte for byte what the serving route's selected integration response
//! > rendered, or byte for byte the `Data` a `PostToConnection` posted — and never a document
//! > the gateway substituted for it.
//!
//! What ENDS such a connection belongs to the suite that owns every refusal of a client
//! message, `websocket_message_refusals.rs`: a message this plane does not serve is answered
//! with a CLOSE frame carrying the RFC 6455 §7.4.1 status code of that refusal, never a bare
//! transport close, and that suite's last case is the one that drives it on a connection which
//! has already been serving. The same division holds with `websocket_lambda_routes.rs`, which
//! owns every frame a route fronting a FUNCTION is answered with — including the gateway's own
//! `Internal server error` frame and the byte-for-byte reply of a function's body; the cases
//! here drive MOCK integrations only, so what they add is the connection's LIFETIME (a
//! sequence over one socket, and a push interleaved with it), not another reading of a route's
//! reply.
//!
//! The rows span what the rule generalises over: the stage's state (deployed, not deployed,
//! absent), the `$connect` route's integration (absent, a MOCK that selects a `2xx`, a MOCK
//! that selects nothing, one fronting a backend), the selecting status (`200`, `404`, a
//! status no pattern matches, none at all), the integration-response key forms (`/2\d\d/`,
//! `/4\d{2}/`, `$default`), whether the serving route is two-way, and the message's own
//! shape (a selected action, an unselected action, no action member, text that is not JSON);
//! and for the connection's lifetime, the message's place in the sequence (first, repeated,
//! after another route's message, after a push) and the payload's own form (a JSON document,
//! text that is no JSON document, a document shaped like the gateway's own envelope, no bytes
//! at all, several kilobytes).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

use crate::api_lifecycle::{as_str, call, emulator};
// The ONE reading, in this test binary, of "is this frame the gateway's own `Internal server
// error` rather than a payload a route produced": the suite that owns the gateway frame holds
// it, and the lifetime cases below ask it of every frame they read.
use crate::websocket_lambda_routes::is_gateway_frame;

/// How long a case waits for one frame before failing: generous for a loaded test machine,
/// and bounded so a case that will never be answered fails instead of hanging the suite.
const FRAME_DEADLINE: Duration = Duration::from_secs(10);

/// How long a case that MUST be answered keeps looking ([`exchanged_reply`]): generous, so a
/// function whose runtime is starting on a loaded host still answers inside it. It bounds no
/// behaviour any case asserts on.
const REPLY_DEADLINE: Duration = Duration::from_secs(120);

/// The status a completed handshake carries (RFC 6455 §4.1).
const SWITCHING_PROTOCOLS: u16 = 101;

/// The statuses a refused handshake carries: the `403` an `execute-api` endpoint answers for
/// a stage it does not serve, the `404` a `$connect` integration's own reply refuses a client
/// with, and the `500` a gateway answers for a connection request its api's configuration
/// cannot be served from.
const FORBIDDEN: u16 = 403;
const NOT_FOUND: u16 = 404;
const INTERNAL_SERVER_ERROR: u16 = 500;

/// The expression AWS documents for a WebSocket api whose messages carry an `action`
/// property, which is the one every api below selects routes by unless a case names another.
pub(crate) const ACTION_SELECTION_EXPRESSION: &str = "$request.body.action";

/// A WebSocket api whose messages select a route by `expression`.
fn websocket_api(name: &str, expression: &str) -> Value {
    json!({
        "name": name,
        "protocolType": "WEBSOCKET",
        "routeSelectionExpression": expression
    })
}

/// One api, created with the socket its `ApiEndpoint` announces.
pub(crate) struct Api {
    pub(crate) api_id: String,
    /// The `host:port` of the api's own socket, read off the announced `ApiEndpoint` rather
    /// than spelled out here, so a case dials the address the service reported.
    pub(crate) authority: String,
}

/// Create a WEBSOCKET api selecting routes by the documented `action` expression, and read
/// the socket address it announces.
pub(crate) async fn create_api(emulator: &MultiServiceRouter, case: &str, name: &str) -> Api {
    create_selecting_api(emulator, case, name, ACTION_SELECTION_EXPRESSION).await
}

/// [`create_api`] for an api selecting routes by `expression`.
pub(crate) async fn create_selecting_api(
    emulator: &MultiServiceRouter,
    case: &str,
    name: &str,
    expression: &str,
) -> Api {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(websocket_api(name, expression)),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {created}"
    );
    let announced = as_str(&created, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
        })
        .to_string();
    Api {
        api_id: as_str(&created, "apiId"),
        authority,
    }
}

/// Create one integration of the api and answer its `IntegrationId`.
pub(crate) async fn create_integration(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    body: Value,
) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(body),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {created}"
    );
    as_str(&created, "integrationId")
}

/// Create one integration response of an integration.
pub(crate) async fn create_integration_response(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    integration_id: &str,
    body: Value,
) {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses"),
        Some(body),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegrationResponse must answer 201, got {created}"
    );
}

/// Create one route over `integration_id`, and a route response for it when `two_way` is
/// set — "to define a two-way route, create a route response".
pub(crate) async fn create_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    integration_id: &str,
    two_way: bool,
) {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": route_key,
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {created}"
    );
    if two_way {
        let route_id = as_str(&created, "routeId");
        let (status, response) = call(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            Some(json!({ "routeResponseKey": "$default" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateRouteResponse must answer 201, got {response}"
        );
    }
}

/// Create a stage, and release a deployment to it when `deploy` is set.
pub(crate) async fn create_stage(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
    deploy: bool,
) {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": stage })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {created}"
    );
    if deploy {
        let (status, deployment) = call(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/deployments"),
            Some(json!({ "stageName": stage })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateDeployment must answer 201, got {deployment}"
        );
    }
}

/// The socket a client gets from a completed handshake.
pub(crate) type ClientSocket = tokio_tungstenite::WebSocketStream<TcpStream>;

/// The loopback host the core binds a resource listener on by default, and so the only
/// address these cases ever dial: a case connects to the emulator's own listener on this
/// machine and nowhere else.
const LOOPBACK_HOST: &str = "127.0.0.1";

/// The port of the api's own announced socket, having checked the address is
/// [`LOOPBACK_HOST`] — so the dial below names that literal and reaches nothing else.
fn loopback_port(case: &str, authority: &str) -> u16 {
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected a host:port, got {authority:?}"));
    assert_eq!(
        host, LOOPBACK_HOST,
        "case {case:?}: a case dials only the emulator's own loopback listener, got \
         {authority:?}"
    );
    port.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a port, got {authority:?}"))
}

/// Dial the api's own announced socket on the loopback address above.
pub(crate) async fn dial_loopback(case: &str, authority: &str) -> std::io::Result<TcpStream> {
    let port = loopback_port(case, authority);
    TcpStream::connect(("127.0.0.1", port)).await
}

/// Open an RFC 6455 handshake against `authority`'s `/{stage}` path, as a client library
/// does: the status a refusal carries, or the established socket.
async fn handshake(
    case: &str,
    authority: &str,
    stage: &str,
) -> Result<ClientSocket, u16> {
    handshake_at(case, authority, &format!("/{stage}")).await
}

/// [`handshake`] against any `path` of the announced socket — a stage, and whatever query
/// string the client carries, which is what an identity source and a `$connect` event read.
pub(crate) async fn handshake_at(
    case: &str,
    authority: &str,
    path: &str,
) -> Result<ClientSocket, u16> {
    let stream = dial_loopback(case, authority).await.unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the announced socket {authority:?} must accept a connection, \
             got {error}"
        )
    });
    let url = format!("ws://{authority}{path}");
    match tokio_tungstenite::client_async(&url, stream).await {
        Ok((socket, response)) => {
            assert_eq!(
                response.status().as_u16(),
                SWITCHING_PROTOCOLS,
                "case {case:?}: a completed handshake must answer 101"
            );
            Ok(socket)
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(error) => panic!("case {case:?}: the handshake failed before any answer: {error}"),
    }
}

/// Send one message and read the frame the api answers with, or `None` when it answers
/// nothing within [`FRAME_DEADLINE`].
pub(crate) async fn exchange(
    case: &str,
    socket: &mut ClientSocket,
    message: &str,
) -> Option<String> {
    sent(case, socket, message).await;
    read_frame(case, socket, FRAME_DEADLINE).await
}

/// [`exchange`] for a case that MUST be answered: the message is sent ONCE and the frame it is
/// answered with is awaited for as long as [`REPLY_DEADLINE`].
///
/// The first invocation of a function starts its runtime, and on a loaded host that start can
/// take longer than [`FRAME_DEADLINE`] — a wait that is not the limit any case here tests, so
/// it is given generous room. The wait is for the api's REPLY, a condition, and nothing
/// asserts on the clock. The message is never sent a second time: a connection carries the
/// reply to each call it was given, so a second send would leave the case reading the FIRST
/// call's reply as the second's.
pub(crate) async fn exchanged_reply(
    case: &str,
    socket: &mut ClientSocket,
    message: &str,
) -> String {
    sent(case, socket, message).await;
    read_frame(case, socket, REPLY_DEADLINE)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the api must answer the message {message:?}"))
}

/// Send one message over the connection.
async fn sent(case: &str, socket: &mut ClientSocket, message: &str) {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the message must be sent, got {error}"));
}

/// Read the next frame the api sends, or `None` when it sends nothing `within` that span.
async fn read_frame(
    case: &str,
    socket: &mut ClientSocket,
    within: Duration,
) -> Option<String> {
    match tokio::time::timeout(within, socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => Some(text.as_str().to_string()),
        Ok(Some(Ok(Message::Binary(bytes)))) => Some(String::from_utf8_lossy(&bytes).to_string()),
        Ok(Some(Ok(other))) => panic!("case {case:?}: unexpected frame {other:?}"),
        Ok(Some(Err(error))) => panic!("case {case:?}: the socket failed: {error}"),
        Ok(None) => panic!("case {case:?}: the socket closed instead of answering"),
        Err(_elapsed) => None,
    }
}

/// The handshake's own answer, over the api's announced socket, for every state the stage
/// and the connection request's route can be in.
#[tokio::test]
async fn the_announced_socket_answers_the_handshake_from_the_apis_records() {
    /// One row: what the api holds, and the status the handshake must be answered with.
    struct Row {
        case: &'static str,
        /// The `$connect` route's integration, when the api has one.
        connect_integration: Option<Value>,
        /// The integration responses that integration holds.
        connect_responses: Vec<Value>,
        deploy: bool,
        /// The stage the handshake names, which is not always the stage that was created.
        handshake_stage: &'static str,
        expected: Result<(), u16>,
    }

    let stage = "prod";
    let rows = vec![
        Row {
            case: "a deployed stage with no $connect route completes the handshake",
            connect_integration: None,
            connect_responses: Vec::new(),
            deploy: true,
            handshake_stage: stage,
            expected: Ok(()),
        },
        Row {
            // The recorded case: a MOCK integration on $connect with no integration
            // response to select, so the connection request has no reply.
            case: "a $connect route bound to a MOCK integration with no integration \
                   response fails the handshake",
            connect_integration: Some(json!({ "integrationType": "MOCK" })),
            connect_responses: Vec::new(),
            deploy: true,
            handshake_stage: stage,
            expected: Err(INTERNAL_SERVER_ERROR),
        },
        Row {
            // A `MOCK` is API Gateway's loopback endpoint for a WebSocket route, so it
            // ANSWERS the connection request from the integration's own records: the status
            // its rendered request template names selects an integration response, and a
            // `2xx` admits the connection exactly as a backend's `2xx` would.
            case: "a $connect route whose MOCK integration selects a 2xx response completes \
                   the handshake",
            connect_integration: Some(json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 200}" }
            })),
            connect_responses: vec![json!({ "integrationResponseKey": "/2\\d\\d/" })],
            deploy: true,
            handshake_stage: stage,
            expected: Ok(()),
        },
        Row {
            // The other side of that reading: a reply outside `2xx` refuses the handshake
            // with the status the integration answered, which for a `MOCK` is the status its
            // own template named.
            case: "a $connect route whose MOCK integration selects a 4xx response refuses \
                   the handshake with that status",
            connect_integration: Some(json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 404}" }
            })),
            connect_responses: vec![json!({ "integrationResponseKey": "/4\\d{2}/" })],
            deploy: true,
            handshake_stage: stage,
            expected: Err(NOT_FOUND),
        },
        Row {
            // A $connect route whose integration fronts a FUNCTION this emulator does not
            // hold: the backend is the one that would admit or refuse the client, and an
            // invocation that reaches no function gives the connection request no decision,
            // so the handshake is refused rather than admitted on a decision nothing made.
            // (A $connect route whose function DOES answer is driven in
            // `websocket_lambda_routes.rs`, where AWS Lambda is mounted beside this
            // service.)
            case: "a $connect route fronting a function this emulator cannot reach fails \
                   the handshake",
            connect_integration: Some(json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:connect",
                "integrationMethod": "POST"
            })),
            connect_responses: Vec::new(),
            deploy: true,
            handshake_stage: stage,
            expected: Err(INTERNAL_SERVER_ERROR),
        },
        Row {
            case: "a stage with no deployment released to it serves nothing",
            connect_integration: None,
            connect_responses: Vec::new(),
            deploy: false,
            handshake_stage: stage,
            expected: Err(FORBIDDEN),
        },
        Row {
            case: "a stage name the api does not hold serves nothing",
            connect_integration: None,
            connect_responses: Vec::new(),
            deploy: true,
            handshake_stage: "no-such-stage",
            expected: Err(FORBIDDEN),
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = emulator();
        let api = create_api(&emulator, case, "handshake-api").await;

        // Every api carries a $default route over a MOCK integration, so the only thing
        // that differs between the rows is the connection request's own route.
        let default_integration = create_integration(
            &emulator,
            case,
            &api.api_id,
            json!({ "integrationType": "MOCK" }),
        )
        .await;
        create_route(
            &emulator,
            case,
            &api.api_id,
            "$default",
            &default_integration,
            false,
        )
        .await;

        if let Some(integration) = row.connect_integration {
            let connect_integration =
                create_integration(&emulator, case, &api.api_id, integration).await;
            for response in row.connect_responses {
                create_integration_response(
                    &emulator,
                    case,
                    &api.api_id,
                    &connect_integration,
                    response,
                )
                .await;
            }
            create_route(
                &emulator,
                case,
                &api.api_id,
                "$connect",
                &connect_integration,
                false,
            )
            .await;
        }

        create_stage(&emulator, case, &api.api_id, stage, row.deploy).await;

        let answered = handshake(case, &api.authority, row.handshake_stage).await;
        match (&row.expected, &answered) {
            (Ok(()), Ok(_socket)) => {}
            (Err(expected), Err(status)) => assert_eq!(
                status, expected,
                "case {case:?}: expected the handshake refused {expected}, got {status}"
            ),
            (Ok(()), Err(status)) => panic!(
                "case {case:?}: expected the handshake completed with {SWITCHING_PROTOCOLS}, \
                 got {status}"
            ),
            (Err(expected), Ok(_socket)) => panic!(
                "case {case:?}: expected the handshake refused {expected}, got \
                 {SWITCHING_PROTOCOLS}"
            ),
        }
    }
}

/// The whole exchange an api that reaches NO backend at all carries: a `$connect` route bound
/// to a `MOCK` whose own records admit the connection, and action routes whose `MOCK`
/// integrations render the reply each frame is answered with on that same socket.
///
/// It is the configuration a client can deploy without any compute service — the one a
/// CloudFormation template of api, integrations, integration responses, routes, route
/// responses, a deployment and a stage describes (`core/router/tests/all/
/// cloudformation_websocket_action_dispatch.rs` deploys exactly that estate) — so the whole
/// of it has to hold over one connection rather than row by row.
#[tokio::test]
async fn a_mock_backed_api_admits_the_connection_and_answers_each_action() {
    /// One row: the message the client sends, and the reply the route it selects renders.
    struct Row {
        case: &'static str,
        message: &'static str,
        expected: &'static str,
    }

    let case = "an api whose every route is a MOCK loopback";
    let emulator = emulator();
    let api = create_api(&emulator, case, "mock-session-api").await;

    // The `$connect` route's own integration: a MOCK that renders a 2xx and holds the
    // response that status selects, which is what admits the connection.
    let connect = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    create_integration_response(
        &emulator,
        case,
        &api.api_id,
        &connect,
        json!({ "integrationResponseKey": "/2\\d\\d/" }),
    )
    .await;
    create_route(&emulator, case, &api.api_id, "$connect", &connect, false).await;

    // One two-way route per action value the api serves, each with the reply its own
    // integration response renders: the two the expression selects by name, and `$default`.
    for (route_key, reply) in [
        ("echo", r#"{"served":"echo"}"#),
        ("status", r#"{"served":"status"}"#),
        ("$default", r#"{"served":"default"}"#),
    ] {
        let integration = create_integration(
            &emulator,
            case,
            &api.api_id,
            json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 200}" }
            }),
        )
        .await;
        create_integration_response(
            &emulator,
            case,
            &api.api_id,
            &integration,
            json!({
                "integrationResponseKey": "/2\\d\\d/",
                "responseTemplates": { "$default": reply }
            }),
        )
        .await;
        create_route(&emulator, case, &api.api_id, route_key, &integration, true).await;
    }
    create_stage(&emulator, case, &api.api_id, "prod", true).await;

    let mut socket = handshake(case, &api.authority, "prod")
        .await
        .unwrap_or_else(|status| {
            panic!(
                "case {case:?}: a MOCK-backed $connect whose records reply 2xx must complete \
                 the handshake, got {status}"
            )
        });

    // The rows span what the dispatch generalises over: each declared action value, an action
    // no route key matches, and a message carrying no action member at all.
    let rows = &[
        Row {
            case: "the first declared action selects its own route",
            message: r#"{"action":"echo","note":"one"}"#,
            expected: r#"{"served":"echo"}"#,
        },
        Row {
            case: "the second declared action selects the other route",
            message: r#"{"action":"status"}"#,
            expected: r#"{"served":"status"}"#,
        },
        Row {
            case: "an action no declared route key matches is served by $default",
            message: r#"{"action":"not-declared"}"#,
            expected: r#"{"served":"default"}"#,
        },
        Row {
            case: "a message carrying no action member is served by $default",
            message: r#"{"note":"no action here"}"#,
            expected: r#"{"served":"default"}"#,
        },
    ];

    for row in rows {
        let answered = exchanged_reply(row.case, &mut socket, row.message).await;
        assert_eq!(
            answered, row.expected,
            "case {:?}: expected the reply {:?}, got {answered:?}",
            row.case, row.expected
        );
    }
}

/// A handshake arriving on the api's own socket names its stage either way a client writes
/// the address: the path's first segment, or the endpoint MARKER a client that rewrote the
/// announced URI into a path carries.
#[tokio::test]
async fn the_announced_socket_reads_the_stage_the_endpoint_marker_carries() {
    /// One row: the path the client's handshake addresses, built from the api's id, and
    /// whether the socket completes it.
    struct Row {
        case: &'static str,
        path: fn(&str) -> String,
        expected: Result<(), u16>,
    }

    /// The stage every row addresses, named once so the stage these rows are created with
    /// and the one their paths spell cannot drift (a `fn` pointer captures nothing, so the
    /// rows read this const rather than a binding).
    const STAGE: &str = "prod";

    // The rows span the spellings of ONE address: the plain stage path, the marker followed
    // by the endpoint PREFIX and the api id, the marker followed by the endpoint HOST, a
    // marker naming another api, and a marker naming no stage of this api. The last two name
    // nothing this socket serves, which is the `403` an `execute-api` endpoint answers for
    // what it does not serve.
    let rows: &[Row] = &[
        Row {
            case: "the plain stage path a client hands a WebSocket library",
            path: |_api_id| format!("/{STAGE}"),
            expected: Ok(()),
        },
        Row {
            case: "the endpoint marker with the endpoint prefix and the api id",
            path: |api_id| format!("/_aws/execute-api/{api_id}/{STAGE}"),
            expected: Ok(()),
        },
        Row {
            case: "the endpoint marker with the endpoint host itself",
            path: |api_id| {
                format!("/_aws/{api_id}.execute-api.us-east-1.amazonaws.com/{STAGE}")
            },
            expected: Ok(()),
        },
        Row {
            case: "the endpoint marker naming another api than the one this socket serves",
            path: |_api_id| format!("/_aws/execute-api/notthisapi/{STAGE}"),
            expected: Err(FORBIDDEN),
        },
        Row {
            case: "the endpoint marker naming no stage of this api",
            path: |api_id| format!("/_aws/execute-api/{api_id}/no-such-stage"),
            expected: Err(FORBIDDEN),
        },
    ];

    let case = "an api whose stage is addressed by every spelling of its own address";
    let emulator = emulator();
    let api = create_api(&emulator, case, "marker-api").await;
    let integration = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({ "integrationType": "MOCK" }),
    )
    .await;
    create_route(&emulator, case, &api.api_id, "$default", &integration, false).await;
    create_stage(&emulator, case, &api.api_id, STAGE, true).await;

    for row in rows {
        let path = (row.path)(&api.api_id);
        let answered = handshake_at(row.case, &api.authority, &path).await;
        match (&row.expected, &answered) {
            (Ok(()), Ok(_socket)) => {}
            (Err(expected), Err(status)) => assert_eq!(
                status, expected,
                "case {:?} ({path}): expected the handshake refused {expected}, got {status}",
                row.case
            ),
            (Ok(()), Err(status)) => panic!(
                "case {:?} ({path}): expected the handshake completed with \
                 {SWITCHING_PROTOCOLS}, got {status}",
                row.case
            ),
            (Err(expected), Ok(_socket)) => panic!(
                "case {:?} ({path}): expected the handshake refused {expected}, got \
                 {SWITCHING_PROTOCOLS}",
                row.case
            ),
        }
    }
}

/// Over an established socket, each message is dispatched to the route the api's selection
/// expression selects and answered by what that route's selected integration response
/// renders.
#[tokio::test]
async fn a_message_is_answered_by_the_integration_response_its_status_selects() {
    /// One row: the message sent, and the reply the client must read back (`None` for a
    /// message the api answers nothing to).
    struct Row {
        case: &'static str,
        message: &'static str,
        expected: Option<&'static str>,
    }

    let case = "three integration responses selected by the synthesized status";
    let emulator = emulator();
    let api = create_api(&emulator, case, "dispatch-api").await;

    // The $default route's MOCK integration synthesizes `200`, and holds three
    // integration responses whose keys span the forms a key takes.
    let defaulted = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    for response in [
        json!({
            "integrationResponseKey": "/2\\d\\d/",
            "responseTemplates": { "$default": "{\"picked\":\"2xx\"}" }
        }),
        json!({
            "integrationResponseKey": "/4\\d{2}/",
            "responseTemplates": { "$default": "{\"picked\":\"4xx\"}" }
        }),
        json!({
            "integrationResponseKey": "$default",
            "responseTemplates": { "$default": "{\"picked\":\"default\"}" }
        }),
    ] {
        create_integration_response(&emulator, case, &api.api_id, &defaulted, response).await;
    }
    create_route(&emulator, case, &api.api_id, "$default", &defaulted, true).await;

    // A second, NAMED route whose MOCK integration synthesizes `404`, so the selecting
    // status — and with it the selected response — differs per route.
    let notfound = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 404}" }
        }),
    )
    .await;
    for response in [
        json!({
            "integrationResponseKey": "/2\\d\\d/",
            "responseTemplates": { "$default": "{\"picked\":\"2xx\"}" }
        }),
        json!({
            "integrationResponseKey": "/4\\d{2}/",
            "responseTemplates": { "$default": "{\"picked\":\"4xx\"}" }
        }),
    ] {
        create_integration_response(&emulator, case, &api.api_id, &notfound, response).await;
    }
    create_route(&emulator, case, &api.api_id, "missing", &notfound, true).await;

    // A ONE-WAY route: it holds no route response, so nothing is sent back for it however
    // its integration replied.
    let one_way = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    create_integration_response(
        &emulator,
        case,
        &api.api_id,
        &one_way,
        json!({
            "integrationResponseKey": "$default",
            "responseTemplates": { "$default": "{\"picked\":\"one-way\"}" }
        }),
    )
    .await;
    create_route(&emulator, case, &api.api_id, "oneway", &one_way, false).await;

    create_stage(&emulator, case, &api.api_id, "prod", true).await;
    let mut socket = handshake(case, &api.authority, "prod")
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));

    let rows = vec![
        Row {
            case: "the selected route's 2xx response renders the reply",
            message: r#"{"action":"$default"}"#,
            expected: Some(r#"{"picked":"2xx"}"#),
        },
        Row {
            case: "a route whose integration synthesizes 404 selects the 4xx response",
            message: r#"{"action":"missing"}"#,
            expected: Some(r#"{"picked":"4xx"}"#),
        },
        Row {
            case: "an action no route key matches is served by $default",
            message: r#"{"action":"not-a-route"}"#,
            expected: Some(r#"{"picked":"2xx"}"#),
        },
        Row {
            case: "a message carrying no action member is served by $default",
            message: r#"{"hello":"world"}"#,
            expected: Some(r#"{"picked":"2xx"}"#),
        },
        Row {
            case: "text that is not JSON selects nothing and is served by $default",
            message: "not json at all",
            expected: Some(r#"{"picked":"2xx"}"#),
        },
        Row {
            case: "a one-way route sends nothing back",
            message: r#"{"action":"oneway"}"#,
            expected: None,
        },
    ];

    for row in rows {
        let answered = exchange(row.case, &mut socket, row.message).await;
        assert_eq!(
            answered.as_deref(),
            row.expected,
            "case {:?}: expected the reply {:?}, got {answered:?}",
            row.case,
            row.expected
        );
    }
}

/// A message the api routes nowhere — no route key is selected and the api has no `$default`
/// route — is answered with the gateway's own `Forbidden` frame, naming the connection and
/// the call.
#[tokio::test]
async fn a_message_no_route_serves_is_answered_with_the_gateways_forbidden_frame() {
    let case = "an api with one named route and no $default route";
    let emulator = emulator();
    let api = create_api(&emulator, case, "unrouted-api").await;
    let integration = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    create_integration_response(
        &emulator,
        case,
        &api.api_id,
        &integration,
        json!({
            "integrationResponseKey": "$default",
            "responseTemplates": { "$default": "{\"picked\":\"named\"}" }
        }),
    )
    .await;
    create_route(&emulator, case, &api.api_id, "named", &integration, true).await;
    create_stage(&emulator, case, &api.api_id, "prod", true).await;

    let mut socket = handshake(case, &api.authority, "prod")
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));

    // The rows span what the rule generalises over: an action the api holds a route for,
    // and one it does not.
    for (label, message, expected) in [
        ("a selected route still serves", r#"{"action":"named"}"#, None),
        (
            "an unselected action with no $default route",
            r#"{"action":"nowhere"}"#,
            Some("Forbidden"),
        ),
    ] {
        let answered = exchange(label, &mut socket, message).await;
        match expected {
            None => assert_eq!(
                answered.as_deref(),
                Some(r#"{"picked":"named"}"#),
                "case {label:?}: expected the route's reply, got {answered:?}"
            ),
            Some(reason) => {
                let frame: Value = serde_json::from_str(
                    answered
                        .as_deref()
                        .unwrap_or_else(|| panic!("case {label:?}: expected a frame, got nothing")),
                )
                .unwrap_or_else(|error| {
                    panic!("case {label:?}: expected a JSON frame, got {answered:?} ({error})")
                });
                assert_eq!(
                    frame["message"].as_str(),
                    Some(reason),
                    "case {label:?}: expected the gateway's {reason:?} frame, got {frame}"
                );
                assert!(
                    frame["connectionId"].as_str().is_some_and(|id| !id.is_empty()),
                    "case {label:?}: the frame must name the connection, got {frame}"
                );
                assert!(
                    frame["requestId"].as_str().is_some_and(|id| !id.is_empty()),
                    "case {label:?}: the frame must name the call, got {frame}"
                );
            }
        }
    }
}

/// Deleting the api closes its socket: the address it announced accepts no connection, which
/// is what makes a deleted api stop answering.
#[tokio::test]
async fn deleting_the_api_closes_the_socket_it_announced() {
    let case = "the api is deleted while its socket is announced";
    let emulator = emulator();
    let api = create_api(&emulator, case, "closing-api").await;
    let integration = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({ "integrationType": "MOCK" }),
    )
    .await;
    create_route(&emulator, case, &api.api_id, "$default", &integration, false).await;
    create_stage(&emulator, case, &api.api_id, "prod", true).await;

    // While the api exists its socket completes the handshake.
    let socket = handshake(case, &api.authority, "prod").await;
    assert!(
        socket.is_ok(),
        "case {case:?}: the announced socket must complete the handshake while the api exists"
    );
    drop(socket);

    let (status, deleted) = call(
        &emulator,
        "DELETE",
        &format!("/v2/apis/{}", api.api_id),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteApi must answer 204, got {status} {deleted}"
    );

    // The port is closed, so a client dialling the announced address is refused. A port the
    // operating system has not yet released may still accept the connection, in which case
    // the handshake itself must not be completed. The address is the loopback one the api
    // announced and this case already dialled above.
    match dial_loopback(case, &api.authority).await {
        Err(_refused) => {}
        Ok(stream) => {
            let url = format!("ws://{}/prod", api.authority);
            let answered = tokio_tungstenite::client_async(&url, stream).await;
            assert!(
                answered.is_err(),
                "case {case:?}: a deleted api's socket must not complete a handshake"
            );
        }
    }
}

/// The host the `@connections` management API is addressed at, and the signing name its
/// requests carry: AWS publishes that API on the api's own invoke endpoint, whose service is
/// `execute-api`.
const CONNECTIONS_HOST: &str = "execute-api.eu-west-1.amazonaws.com";

/// The stage the api below deploys, which is the segment its `@connections` callback URL
/// carries: these operations are published at `…/{stage}/@connections/{id}`, and an endpoint
/// that names no deployed stage serves nothing at all.
const CONNECTIONS_STAGE: &str = "prod";

/// A router serving the api's control plane AND the `@connections` management API over ONE
/// engine, which is how a connection the api's socket carries is the connection that API
/// addresses.
fn emulator_with_connections() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One `@connections` call, signed for the data plane's own service: its status and body.
async fn connections_call(
    emulator: &MultiServiceRouter,
    method: &str,
    connection_id: &str,
    payload: Option<&str>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://{CONNECTIONS_HOST}/{CONNECTIONS_STAGE}/@connections/{connection_id}"
        ))
        .header("host", CONNECTIONS_HOST)
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/eu-west-1/execute-api/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .body(Bytes::from(payload.unwrap_or_default().to_string()))
        .expect("the test request is well formed");
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

/// A connection the api's socket carries is the connection the `@connections` management API
/// addresses: a post to it is delivered over that socket, and once the client closes, each of
/// the three operations answers `GoneException`.
#[tokio::test]
async fn a_carried_connection_is_posted_to_and_then_gone_after_the_client_closes() {
    let case = "a live connection addressed through @connections";
    let emulator = emulator_with_connections();
    let api = create_api(&emulator, case, "connections-api").await;

    // The $default route is two-way and renders the CONNECTION's own id, which is how a
    // client learns the id AWS gives its handler in `$context.connectionId`.
    let integration = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    create_integration_response(
        &emulator,
        case,
        &api.api_id,
        &integration,
        json!({
            "integrationResponseKey": "$default",
            "responseTemplates": { "$default": "$context.connectionId" }
        }),
    )
    .await;
    create_route(&emulator, case, &api.api_id, "$default", &integration, true).await;
    create_stage(&emulator, case, &api.api_id, CONNECTIONS_STAGE, true).await;

    let mut socket = handshake(case, &api.authority, CONNECTIONS_STAGE)
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));
    let connection_id = exchange(case, &mut socket, r#"{"action":"whoami"}"#)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the route must reply with the connection id"));
    assert!(
        !connection_id.is_empty(),
        "case {case:?}: the connection must have an id, got {connection_id:?}"
    );

    // GetConnection reports the live connection.
    let (status, reported) = connections_call(&emulator, "GET", &connection_id, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetConnection must report the live connection, got {status} {reported}"
    );

    // A post is DELIVERED over the socket: the client reads exactly the payload sent.
    let payload = r#"{"hello":"socket"}"#;
    let (status, posted) =
        connections_call(&emulator, "POST", &connection_id, Some(payload)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PostToConnection must answer 200, got {status} {posted}"
    );
    let delivered = match tokio::time::timeout(FRAME_DEADLINE, socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text.as_str().to_string(),
        Ok(Some(Ok(Message::Binary(bytes)))) => String::from_utf8_lossy(&bytes).to_string(),
        other => panic!("case {case:?}: the posted payload must arrive, got {other:?}"),
    };
    assert_eq!(
        delivered, payload,
        "case {case:?}: expected the posted payload {payload:?}, got {delivered:?}"
    );

    // The client closes. From then on the connection no longer exists, so each of the three
    // operations answers GoneException (HTTP 410). The close travels over the socket, so the
    // answer is polled rather than assumed immediate.
    socket
        .close(None)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the client must close, got {error}"));
    drop(socket);

    let deadline = tokio::time::Instant::now() + FRAME_DEADLINE;
    loop {
        let (status, _body) = connections_call(&emulator, "GET", &connection_id, None).await;
        if status == GONE {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {GONE} once the client closed, got {status}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The rows span the three operations the management API models over one connection.
    for (method, payload) in [
        ("GET", None),
        ("POST", Some(r#"{"after":"close"}"#)),
        ("DELETE", None),
    ] {
        let (status, body) = connections_call(&emulator, method, &connection_id, payload).await;
        assert_eq!(
            status, GONE,
            "case {method:?}: expected {GONE} for a connection that has closed, got {status} \
             {body}"
        );
    }
}

/// The status `GoneException` carries: "The connection with the provided id no longer
/// exists".
const GONE: u16 = 410;

/// The stage every selection case below deploys and dials.
const SELECTION_STAGE: &str = "prod";

/// The route key that serves a message the expression selects no route for.
const DEFAULT_ROUTE_KEY: &str = "$default";

/// What the `$default` route's own reply NAMES it: a mapping template reads `$` as the start
/// of a variable reference, so the name a template renders carries no `$`.
fn served_by(route_key: &str) -> &str {
    route_key.trim_start_matches('$')
}

/// Create an api selecting routes by `expression`, hold one TWO-WAY route per key in
/// `route_keys`, and release a deployment to [`SELECTION_STAGE`].
///
/// Each route's MOCK integration renders `{"picked":"<route key>"}`, so the reply a client
/// reads back NAMES the route the frame was dispatched to — which is what a selection case
/// asserts on.
async fn selecting_api(
    emulator: &MultiServiceRouter,
    case: &str,
    expression: &str,
    route_keys: &[&str],
) -> Api {
    let api = create_selecting_api(emulator, case, "selection-api", expression).await;
    for route_key in route_keys {
        let name = served_by(route_key);
        let integration = create_integration(
            emulator,
            case,
            &api.api_id,
            json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 200}" }
            }),
        )
        .await;
        create_integration_response(
            emulator,
            case,
            &api.api_id,
            &integration,
            json!({
                "integrationResponseKey": "$default",
                "responseTemplates": {
                    "$default": format!("{{\"picked\":\"{name}\"}}")
                }
            }),
        )
        .await;
        create_route(emulator, case, &api.api_id, route_key, &integration, true).await;
    }
    create_stage(emulator, case, &api.api_id, SELECTION_STAGE, true).await;
    api
}

/// The route key the reply NAMES — which route the frame was dispatched to.
fn picked(case: &str, answered: Option<String>) -> String {
    let frame = answered
        .unwrap_or_else(|| panic!("case {case:?}: the api must answer the frame, got nothing"));
    let parsed: Value = serde_json::from_str(&frame).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a JSON frame, got {frame:?} ({error})")
    });
    parsed["picked"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the serving route named, got {parsed}")
        })
        .to_string()
}

/// A frame is routed by the value the api's expression PATH reaches in it, however deep that
/// path goes, and by `$default` when it reaches none.
#[tokio::test]
async fn a_frame_selects_its_route_by_the_path_its_apis_expression_names() {
    /// One row: the api's expression, the frame sent, and the route key that must serve it.
    struct Row {
        case: &'static str,
        expression: &'static str,
        frame: &'static str,
        expected: &'static str,
    }

    // The rows differ along what the rule generalises over: the DEPTH of the path (the one
    // member AWS documents, two members, three), the spelling of the reference (bare and
    // braced), and every way a path reaches no key — an absent member, a member reached
    // through a value that is no object, a value that is no route key, and a frame that is no
    // JSON object at all.
    let rows = [
        Row {
            case: "the one-member path AWS documents",
            expression: ACTION_SELECTION_EXPRESSION,
            frame: r#"{"action":"alpha"}"#,
            expected: "alpha",
        },
        Row {
            case: "a path one member deeper",
            expression: "$request.body.meta.kind",
            frame: r#"{"meta":{"kind":"beta"},"text":"hello"}"#,
            expected: "beta",
        },
        Row {
            case: "a path three members deep",
            expression: "$request.body.outer.inner.kind",
            frame: r#"{"outer":{"inner":{"kind":"alpha"}}}"#,
            expected: "alpha",
        },
        Row {
            case: "the braced spelling of a nested path",
            expression: "${request.body.meta.kind}",
            frame: r#"{"meta":{"kind":"beta"}}"#,
            expected: "beta",
        },
        Row {
            case: "a nested path the frame carries no value for",
            expression: "$request.body.meta.kind",
            frame: r#"{"meta":{"other":"beta"}}"#,
            expected: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "a nested path reached through a value that is no object",
            expression: "$request.body.meta.kind",
            frame: r#"{"meta":"beta"}"#,
            expected: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "a nested path reaching a key no route of the api carries",
            expression: "$request.body.meta.kind",
            frame: r#"{"meta":{"kind":"gamma"}}"#,
            expected: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "a path stopping at an object, which names no route key",
            expression: "$request.body.meta",
            frame: r#"{"meta":{"kind":"beta"}}"#,
            expected: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "a frame that is no JSON object",
            expression: "$request.body.meta.kind",
            frame: "not json at all",
            expected: DEFAULT_ROUTE_KEY,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = emulator();
        let api = selecting_api(
            &emulator,
            case,
            row.expression,
            &["alpha", "beta", DEFAULT_ROUTE_KEY],
        )
        .await;
        let mut socket = handshake(case, &api.authority, SELECTION_STAGE)
            .await
            .unwrap_or_else(|status| {
                panic!("case {case:?}: the handshake must complete, got {status}")
            });
        let served = picked(case, exchange(case, &mut socket, row.frame).await);
        assert_eq!(
            served,
            served_by(row.expected),
            "case {case:?}: expected {:?} to be served by {:?} under {:?}, got {served:?}",
            row.frame,
            row.expected,
            row.expression
        );
    }
}

/// An api whose `RouteSelectionExpression` is changed and whose stage is redeployed selects
/// routes by the NEW expression for the frames that follow.
#[tokio::test]
async fn a_redeployed_stage_selects_routes_by_the_apis_updated_expression() {
    let case = "the api's RouteSelectionExpression is replaced and the stage redeployed";
    let emulator = emulator();
    let api = selecting_api(
        &emulator,
        case,
        ACTION_SELECTION_EXPRESSION,
        &["alpha", "beta", DEFAULT_ROUTE_KEY],
    )
    .await;
    let mut socket = handshake(case, &api.authority, SELECTION_STAGE)
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));

    // The expression the api was created with routes the first frame.
    let served = picked(case, exchange(case, &mut socket, r#"{"action":"beta"}"#).await);
    assert_eq!(
        served,
        served_by("beta"),
        "case {case:?}: expected the created expression to route the frame to \"beta\", got \
         {served:?}"
    );

    let updated_expression = "$request.body.meta.kind";
    let (status, updated) = call(
        &emulator,
        "PATCH",
        &format!("/v2/apis/{}", api.api_id),
        Some(json!({ "routeSelectionExpression": updated_expression })),
    )
    .await;
    // The status a patch of an api answers here is the one `mutating_success_status.rs` owns.
    assert_eq!(
        status, 201,
        "case {case:?}: UpdateApi must answer 201, got {status} {updated}"
    );
    assert_eq!(
        updated["routeSelectionExpression"].as_str(),
        Some(updated_expression),
        "case {case:?}: the api must report the expression it now carries, got {updated}"
    );
    let (status, deployment) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{}/deployments", api.api_id),
        Some(json!({ "stageName": SELECTION_STAGE })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateDeployment must answer 201, got {status} {deployment}"
    );

    // The rows span what the new expression changes: each route reached under the new path,
    // and a frame keyed the old way, which the new path finds nothing in.
    for (label, frame, expected) in [
        (
            "a frame keyed by the new path reaches that route",
            r#"{"meta":{"kind":"beta"},"text":"hello"}"#,
            "beta",
        ),
        (
            "another route under the same new path",
            r#"{"meta":{"kind":"alpha"}}"#,
            "alpha",
        ),
        (
            "a frame keyed the way the old expression named",
            r#"{"action":"beta"}"#,
            DEFAULT_ROUTE_KEY,
        ),
    ] {
        let served = picked(label, exchange(label, &mut socket, frame).await);
        assert_eq!(
            served,
            served_by(expected),
            "case {label:?}: expected {frame:?} served by {expected:?} under \
             {updated_expression:?}, got {served:?}"
        );
    }
}

/// Each route of [`lasting_api`]: its `RouteKey`, and what its MOCK integration's `$default`
/// integration response renders.
///
/// `whoami` renders the CONNECTION's own id, which is how a client learns the id the
/// `@connections` calls address; every other route renders a payload of its own, spanning a
/// JSON document and text that is no JSON document, so a frame read back names the route that
/// produced it.
const LASTING_ROUTES: [(&str, &str); 4] = [
    ("whoami", "$context.connectionId"),
    ("alpha", r#"{"route":"alpha"}"#),
    ("beta", "beta route plain text, which is no JSON document"),
    (DEFAULT_ROUTE_KEY, r#"{"route":"default"}"#),
];

/// What the route keyed `route_key` renders.
fn rendered_by(case: &str, route_key: &str) -> &'static str {
    LASTING_ROUTES
        .iter()
        .find_map(|(key, rendered)| (*key == route_key).then_some(*rendered))
        .unwrap_or_else(|| panic!("case {case:?}: no route of the api is keyed {route_key:?}"))
}

/// A deployed WEBSOCKET api holding one TWO-WAY route per entry of [`LASTING_ROUTES`], each
/// over a MOCK integration that selects a `2xx` and renders that entry's payload.
async fn lasting_api(emulator: &MultiServiceRouter, case: &str) -> Api {
    let api = create_api(emulator, case, "lasting-connection-api").await;
    for (route_key, rendered) in LASTING_ROUTES {
        let integration = create_integration(
            emulator,
            case,
            &api.api_id,
            json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 200}" }
            }),
        )
        .await;
        create_integration_response(
            emulator,
            case,
            &api.api_id,
            &integration,
            json!({
                "integrationResponseKey": "$default",
                "responseTemplates": { "$default": rendered }
            }),
        )
        .await;
        create_route(emulator, case, &api.api_id, route_key, &integration, true).await;
    }
    create_stage(emulator, case, &api.api_id, CONNECTIONS_STAGE, true).await;
    api
}

/// The message that reaches the `whoami` route, which renders the connection's own id.
fn whoami() -> String {
    json!({ "action": "whoami" }).to_string()
}

/// An established connection carries a SEQUENCE of messages: each is dispatched to the route
/// the api's expression selects for it and answered with exactly what that route rendered, the
/// connection stays the one the handshake established, and no frame is the gateway's own
/// envelope.
#[tokio::test]
async fn an_established_connection_carries_every_message_of_a_sequence() {
    /// One row: the message sent, and the route key whose payload must come back.
    struct Row {
        case: &'static str,
        message: String,
        served_by: &'static str,
    }

    // The rows differ along the message's place in the sequence (first, repeated, after
    // another route's message) and its own shape (an action naming a route, an action naming
    // none, an action that is not text, no action member, text that is no JSON document, a
    // message of several kilobytes).
    let rows = vec![
        Row {
            case: "an action naming a route of the api",
            message: json!({ "action": "alpha" }).to_string(),
            served_by: "alpha",
        },
        Row {
            case: "the same action again, over the same connection",
            message: json!({ "action": "alpha" }).to_string(),
            served_by: "alpha",
        },
        Row {
            case: "another route's action, over the same connection",
            message: json!({ "action": "beta" }).to_string(),
            served_by: "beta",
        },
        Row {
            case: "a message carrying no action member",
            message: json!({ "hello": "world" }).to_string(),
            served_by: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "text that is no JSON document",
            message: "not json at all".to_string(),
            served_by: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "an action naming a route the api does not hold",
            message: json!({ "action": "nosuchroute" }).to_string(),
            served_by: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "an action whose value is not text",
            message: json!({ "action": 7 }).to_string(),
            served_by: DEFAULT_ROUTE_KEY,
        },
        Row {
            case: "several kilobytes of message, inside the frame quota",
            message: json!({ "action": "alpha", "filler": "x".repeat(8 * 1024) }).to_string(),
            served_by: "alpha",
        },
    ];

    let case = "a sequence of messages over one established connection";
    let emulator = emulator();
    let api = lasting_api(&emulator, case).await;
    let mut socket = handshake(case, &api.authority, CONNECTIONS_STAGE)
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));
    let connection_id = exchanged_reply(case, &mut socket, &whoami()).await;
    assert!(
        !connection_id.is_empty(),
        "case {case:?}: the connection must have an id, got {connection_id:?}"
    );
    // The id is the one the connection HAS, not the template that names it: a reply equal to
    // the template text would leave the comparison below a literal against itself.
    assert_ne!(
        connection_id,
        rendered_by(case, "whoami"),
        "case {case:?}: the route must render the connection's own id, not the template text"
    );

    for row in rows {
        let case = row.case;
        let answered = exchanged_reply(case, &mut socket, &row.message).await;
        assert!(
            !is_gateway_frame(&answered),
            "case {case:?}: expected the payload the serving route rendered, got the gateway's \
             own error frame {answered:?}"
        );
        let owed = rendered_by(case, row.served_by);
        assert_eq!(
            answered, owed,
            "case {case:?}: expected the payload route {:?} renders, {owed:?}, got {answered:?}",
            row.served_by
        );
    }

    // Every message of the sequence was carried by ONE connection: the id it reports after the
    // whole sequence is the id it reported before it.
    let still = exchanged_reply(case, &mut socket, &whoami()).await;
    assert_eq!(
        still, connection_id,
        "case {case:?}: the sequence must be carried by the connection the handshake \
         established, {connection_id:?}, got {still:?}"
    );
}

/// A `PostToConnection` to a connection that has been serving messages is delivered over it as
/// EXACTLY the bytes posted, and the connection keeps serving its routes afterwards.
#[tokio::test]
async fn a_push_to_a_serving_connection_is_delivered_as_exactly_the_bytes_posted() {
    let case = "pushes interleaved with the messages a connection serves";
    let emulator = emulator_with_connections();
    let api = lasting_api(&emulator, case).await;
    let mut socket = handshake(case, &api.authority, CONNECTIONS_STAGE)
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));
    let connection_id = exchanged_reply(case, &mut socket, &whoami()).await;

    // The connection is serving before any push: a message of its own is answered by its route.
    let served = exchanged_reply(case, &mut socket, &json!({ "action": "alpha" }).to_string()).await;
    assert_eq!(
        served,
        rendered_by(case, "alpha"),
        "case {case:?}: the connection must serve its routes, got {served:?}"
    );

    // The rows differ along the payload's own form: a JSON document, text that is no JSON
    // document, a document carrying the very member names the gateway's own envelope carries
    // (with the caller's values, so what arrives names who produced it), no bytes at all, and
    // several kilobytes.
    let kilobytes = "x".repeat(8 * 1024);
    let rows: [(&str, &str); 5] = [
        ("a JSON document", r#"{"marker":"posted-by-the-caller"}"#),
        ("text that is no JSON document", "a plain text push"),
        (
            "a document shaped like the gateway's envelope, carrying the caller's own values",
            r#"{"connectionId":"posted-by-the-caller","message":"posted by the caller","requestId":"posted-by-the-caller"}"#,
        ),
        ("no bytes at all", ""),
        ("several kilobytes", kilobytes.as_str()),
    ];

    for (label, payload) in rows {
        let (status, posted) =
            connections_call(&emulator, "POST", &connection_id, Some(payload)).await;
        assert_eq!(
            status, 200,
            "case {label:?}: PostToConnection must answer 200, got {status} {posted}"
        );
        let delivered = read_frame(label, &mut socket, REPLY_DEADLINE)
            .await
            .unwrap_or_else(|| {
                panic!("case {label:?}: the posted payload must arrive over the connection")
            });
        assert!(
            !is_gateway_frame(&delivered),
            "case {label:?}: expected the bytes the caller posted, got the gateway's own error \
             frame {delivered:?}"
        );
        assert_eq!(
            delivered, payload,
            "case {label:?}: expected the posted bytes {payload:?}, got {delivered:?}"
        );
    }

    // And the pushes left the connection serving: a message of its own is still answered by its
    // route.
    let served = exchanged_reply(case, &mut socket, &json!({ "action": "beta" }).to_string()).await;
    assert_eq!(
        served,
        rendered_by(case, "beta"),
        "case {case:?}: the connection must still serve its routes after a push, got {served:?}"
    );
}

