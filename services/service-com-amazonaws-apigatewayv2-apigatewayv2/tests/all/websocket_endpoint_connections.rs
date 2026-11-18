// SPDX-License-Identifier: CC0-1.0

//! The `@connections` management API served on a WEBSOCKET api's OWN socket — the address
//! that api announces as its `ApiEndpoint` — driven over a real connection.
//!
//! The rule these cases assert, one level above the recorded case (one deployed stage, one
//! token of the issued shape nothing had minted):
//!
//! > A WebSocket api's endpoint carries TWO things: the RFC 6455 opening handshake a client
//! > dials, and the `@connections` management API a backend calls, which AWS publishes on
//! > that same endpoint (`…/{stage}/@connections/{id}`). So a request on the api's socket
//! > that is not an opening handshake is served as the HTTP request it is and is ALWAYS
//! > answered with a complete HTTP response — the modelled answer of the service that owns
//! > the `@connections` path space, computed from the connection registry this socket
//! > registers its connections in: `200`/`204` for a connection open on this socket, `410`
//! > `GoneException` for an id of the shape API Gateway issues that names none here (never
//! > minted, closed, or another api's), `400` `BadRequestException` for a value that could
//! > never have been issued, and a bare `413` for a payload past the modelled maximum.
//! > Never a closed connection with no status line, which every SDK retries.
//!
//! The rows span what the rule generalises over: the operation (all three), the
//! connection-id form (the issued shape, and refused by alphabet, padding, length and the
//! format marker),
//! the connection's state (live, closed, another api's), the payload size (empty, short,
//! exactly the modelled maximum, one byte past it) and the payload's bytes (text and
//! non-UTF-8).
//!
//! What the served `200` CARRIES for a live connection is the second rule here:
//!
//! > The report of a connection open on this socket is that connection's OWN record, read
//! > back and nothing else: `identity.sourceIp` the address its client dialled from,
//! > `identity.userAgent` the `User-Agent` field its opening handshake named itself with — and
//! > NO `userAgent` member at all for a handshake that named none — `connectedAt` the instant
//! > that handshake established it and `lastActiveAt` never before that instant. Activity is
//! > what moves `lastActiveAt`: a payload delivered over the connection advances it, and
//! > REPORTING the connection advances nothing, so two reports with no traffic between them
//! > answer identically.
//!
//! Those rows differ along whether the handshake named a user agent at all and along the FORM
//! of the field it named itself with, and the activity case drives the two directions
//! separately: reads, then a delivered push.
//!
//! The handshake itself, the frames a client exchanges and the `$connect`/`$disconnect`
//! routes are `websocket_transport_data_plane`'s and `websocket_connect_backend`'s; this
//! suite drives the socket's OTHER half.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use futures_util::StreamExt;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

use crate::api_lifecycle::{as_str, call};
use crate::path_label::addressed;
use crate::websocket_transport_data_plane::{dial_loopback, handshake_at, ClientSocket};

/// How long a case waits for one frame before failing: generous for a loaded test machine —
/// these suites run beside every other test of the workspace, and a frame that has already
/// been answered `200` can still be scheduled late on a host under that much contention — and
/// bounded so a case that will never be answered fails instead of hanging the suite. The wait
/// is for the FRAME, a condition, and nothing asserts on the clock.
const FRAME_DEADLINE: Duration = Duration::from_secs(120);

/// The `Data` maximum the `@connections` model declares (`@length(min: 0, max: 131072)` on
/// the `Data` blob). Read from the owning crate's constant so the two cannot disagree.
///
/// Shared with `websocket_push_framing`, which reads the frames a payload of that size
/// travels in.
pub(crate) const MAX_DATA_BYTES: usize =
    service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::ops::post_to_connection::MAX_DATA_BYTES;

/// The stage every case deploys.
pub(crate) const STAGE: &str = "prod";

/// An emulator serving this service's control plane AND the `@connections` management API
/// over ONE engine, which is how a connection the api's socket carries is the connection
/// that API addresses.
pub(crate) fn emulator_with_connections() -> MultiServiceRouter {
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

/// One deployed WebSocket api, with the socket its `ApiEndpoint` announces.
pub(crate) struct Deployed {
    /// The `host:port` of the api's own socket, read off the announced `ApiEndpoint` rather
    /// than spelled out here.
    pub(crate) authority: String,
}

/// Create a WEBSOCKET api whose `$default` route renders the connection's own id, deploy it
/// to [`STAGE`], and answer the socket it is reached at.
///
/// The `$default` route is how a client learns the id AWS gives its handler in
/// `$context.connectionId`, which is the id the `@connections` calls below address.
pub(crate) async fn deploy(emulator: &MultiServiceRouter, case: &str, name: &str) -> Deployed {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        })),
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
    let api_id = as_str(&created, "apiId");

    let (status, integration) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {integration}"
    );
    let integration_id = as_str(&integration, "integrationId");
    let (status, response) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses"),
        Some(json!({
            "integrationResponseKey": "$default",
            "responseTemplates": { "$default": "$context.connectionId" }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegrationResponse must answer 201, got {response}"
    );
    let (status, route) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": "$default",
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {route}"
    );
    let route_id = as_str(&route, "routeId");
    let (status, route_response) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
        Some(json!({ "routeResponseKey": "$default" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRouteResponse must answer 201, got {route_response}"
    );
    let (status, stage) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": STAGE })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {stage}"
    );
    let (status, deployment) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({ "stageName": STAGE })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateDeployment must answer 201, got {deployment}"
    );

    Deployed { authority }
}

/// What one `@connections` call was answered: everything a client reads before it looks at
/// the body, and the body.
pub(crate) struct Answer {
    pub(crate) status: u16,
    /// The `x-amzn-errortype` the answer named, or `"<absent>"` — the field a restJson1
    /// client resolves an error SHAPE from.
    pub(crate) error_type: String,
    pub(crate) body: Vec<u8>,
}

impl Answer {
    /// Whether the body names an error SHAPE at all.
    ///
    /// A restJson1 failure names its shape in `x-amzn-errortype` and its BODY carries the
    /// error shape's own modelled members and nothing else, so the AWS-JSON-family `__type`
    /// discriminator is never in it (the management API's `rest_json_error_body` rule). A
    /// body that carries one is the defect this reads for.
    fn names_a_shape_in_the_body(&self) -> bool {
        let value: Value = serde_json::from_slice(&self.body).unwrap_or(Value::Null);
        value.get("__type").is_some()
    }

    /// The `message` a restJson1 error body carries, or `"<no message>"`.
    pub(crate) fn message(&self) -> String {
        let value: Value = serde_json::from_slice(&self.body).unwrap_or(Value::Null);
        value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("<no message>")
            .to_string()
    }
}

/// Make one `@connections` call ON THE API'S OWN SOCKET, as a backend's SDK client does:
/// the documented `…/{stage}/@connections/{id}` path, signed for the data plane's own
/// service, spoken as the HTTP/1.1 it is over the same address the handshake is made at.
///
/// The request is written and the answer read by hand because the point of these cases is
/// what arrives on the WIRE — that a complete HTTP response arrives at all.
pub(crate) async fn connections_call(
    case: &str,
    authority: &str,
    method: &str,
    connection_id: &str,
    body: &[u8],
) -> Answer {
    let mut stream = dial_loopback(case, authority)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the socket must accept a connection: {error}"));
    let head = format!(
        "{method} /{STAGE}/@connections/{connection_id} HTTP/1.1\r\n\
         host: {authority}\r\n\
         authorization: AWS4-HMAC-SHA256 \
         Credential=AKIDTESTAPIGWV2/20261110/us-east-1/execute-api/aws4_request, \
         SignedHeaders=host, Signature=0\r\n\
         content-length: {}\r\n\
         connection: close\r\n\r\n",
        body.len()
    );
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    stream
        .write_all(&request)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must be sent: {error}"));
    let mut answered = Vec::new();
    stream
        .read_to_end(&mut answered)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the answer must be read: {error}"));
    parse(case, &answered)
}

/// The answer those bytes state. A reply with no status line at all is the failure these
/// cases exist to catch, so it panics naming what arrived.
fn parse(case: &str, answered: &[u8]) -> Answer {
    let split = answered
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a complete HTTP response, got {:?}",
                String::from_utf8_lossy(answered)
            )
        });
    let head = String::from_utf8_lossy(&answered[..split]).to_string();
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {status_line:?}"));
    let error_type = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("x-amzn-errortype")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_else(|| "<absent>".to_string());
    Answer {
        status,
        error_type,
        body: answered[split + 4..].to_vec(),
    }
}

/// Open a connection on the api's socket and learn the `ConnectionId` the handshake minted,
/// as a client does: the `$default` route renders it.
pub(crate) async fn open_connection(case: &str, authority: &str) -> (ClientSocket, String) {
    let mut socket = handshake_at(case, authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));
    let connection_id = exchange_text(case, &mut socket, r#"{"action":"whoami"}"#).await;
    assert!(
        !connection_id.is_empty(),
        "case {case:?}: the connection must have an id"
    );
    (socket, connection_id)
}

/// Send one message and read the text frame the api answers with.
async fn exchange_text(case: &str, socket: &mut ClientSocket, message: &str) -> String {
    use futures_util::SinkExt;

    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the message must be sent: {error}"));
    match next_frame(case, socket).await {
        Message::Text(text) => text.as_str().to_string(),
        other => panic!("case {case:?}: expected a text frame, got {other:?}"),
    }
}

/// The next frame the client reads, or a panic naming the case when none arrives within
/// [`FRAME_DEADLINE`].
pub(crate) async fn next_frame(case: &str, socket: &mut ClientSocket) -> Message {
    match tokio::time::timeout(FRAME_DEADLINE, socket.next()).await {
        Ok(Some(Ok(frame))) => frame,
        Ok(Some(Err(error))) => panic!("case {case:?}: the socket failed: {error}"),
        Ok(None) => panic!("case {case:?}: the socket closed instead of carrying a frame"),
        Err(_elapsed) => panic!("case {case:?}: no frame arrived within {FRAME_DEADLINE:?}"),
    }
}

/// A `ConnectionId` of the shape API Gateway issues — base64 of
/// `CONNECTION_ID_BYTES` bytes carrying the format marker its encoding stamps — carrying
/// `label`, so each case names its own. Built by the `@connections` registry's own encoder,
/// the one place such a token is rendered.
pub(crate) fn minted(label: &str) -> String {
    service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::state::connection_id_for(
        label.as_bytes(),
    )
}

/// Every `@connections` call on a deployed stage's own socket is ANSWERED, and the answer is
/// decided by the `ConnectionId`'s shape and the registry: this is the table with NO
/// connection open, so each row is one of the two refusals.
#[tokio::test]
async fn a_connections_call_on_the_apis_socket_is_answered_from_the_registry() {
    // (case, the id as the request sends it, the id the service reads, status, error shape)
    let well_formed = minted("unknown-id");
    let rows: [(&str, String, String, u16, &str); 6] = [
        (
            "an id of the issued shape that names no connection",
            well_formed.clone(),
            well_formed,
            410,
            "GoneException",
        ),
        (
            "an all-A id: eleven zero bytes, so no format marker",
            "AAAAAAAAAAAAAAA=".to_string(),
            "AAAAAAAAAAAAAAA=".to_string(),
            400,
            "BadRequestException",
        ),
        (
            "a single character",
            "x".to_string(),
            "x".to_string(),
            400,
            "BadRequestException",
        ),
        (
            "text that is not base64",
            "not%20base64%21%21".to_string(),
            "not base64!!".to_string(),
            400,
            "BadRequestException",
        ),
        (
            "an unpadded token",
            "Fabc1cWIPHcCJ7A".to_string(),
            "Fabc1cWIPHcCJ7A".to_string(),
            400,
            "BadRequestException",
        ),
        (
            "a 200-character token",
            "A".repeat(200),
            "A".repeat(200),
            400,
            "BadRequestException",
        ),
    ];

    let emulator = emulator_with_connections();
    let api = deploy(&emulator, "the answered table", "answered-api").await;

    for (row, sent, read, expected_status, expected_shape) in rows {
        for method in ["GET", "POST", "DELETE"] {
            let case = format!("{method} on {row}");
            let payload: &[u8] = if method == "POST" { b"push" } else { b"" };
            let answer =
                connections_call(&case, &api.authority, method, &sent, payload).await;
            assert_eq!(
                answer.status, expected_status,
                "case {case:?}: expected HTTP {expected_status}, got {} {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_type, expected_shape,
                "case {case:?}: expected x-amzn-errortype {expected_shape:?}, got {:?}",
                answer.error_type
            );
            assert!(
                !answer.names_a_shape_in_the_body(),
                "case {case:?}: a restJson1 body carries the error shape's own members and \
                 not the shape, which travels in x-amzn-errortype, got {:?}",
                String::from_utf8_lossy(&answer.body)
            );
            if expected_shape == "BadRequestException" {
                assert_eq!(
                    answer.message(),
                    format!("Invalid connectionId: {read}"),
                    "case {case:?}: the refusal must name the id the request sent"
                );
            } else {
                assert_eq!(
                    answer.message(),
                    "<no message>",
                    "case {case:?}: GoneException is memberless, so it carries no message"
                );
            }
        }
    }
}

/// A connection OPEN on the api's socket is the connection its `@connections` calls address:
/// `GetConnection` reports it, `PostToConnection` delivers the exact bytes to that client as
/// a text frame, and `DeleteConnection` closes the socket and makes every later call gone.
#[tokio::test]
async fn a_live_connection_is_reported_posted_to_and_then_closed() {
    let case = "a live connection on the api's socket";
    let emulator = emulator_with_connections();
    let api = deploy(&emulator, case, "live-api").await;
    let (mut socket, connection_id) = open_connection(case, &api.authority).await;

    // GetConnection reports the live connection, from the registry the socket registered
    // it in.
    let answer = connections_call(case, &api.authority, "GET", &connection_id, b"").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetConnection must answer 200, got {} {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let reported: Value = serde_json::from_slice(&answer.body)
        .unwrap_or_else(|error| panic!("case {case:?}: the report must be JSON: {error}"));
    for member in ["connectedAt", "lastActiveAt"] {
        assert!(
            reported.get(member).and_then(Value::as_str).is_some(),
            "case {case:?}: {member} must be reported as an ISO 8601 date-time, got {reported}"
        );
    }
    assert!(
        reported
            .pointer("/identity/sourceIp")
            .and_then(Value::as_str)
            .is_some_and(|ip| !ip.is_empty()),
        "case {case:?}: identity.sourceIp must be reported, got {reported}"
    );

    // Each payload is DELIVERED to that client, byte for byte, as a text frame. The rows
    // span the sizes and the byte classes the modelled `Data` blob allows.
    let payloads: [(&str, Vec<u8>); 4] = [
        ("a short text payload", b"{\"hello\":\"socket\"}".to_vec()),
        ("the empty payload", Vec::new()),
        ("non-UTF-8 bytes", vec![0xff, 0xfe, b'o', b'k']),
        ("exactly the modelled maximum", vec![b'x'; MAX_DATA_BYTES]),
    ];
    for (row, payload) in payloads {
        let case = format!("{case}: {row}");
        let answer =
            connections_call(&case, &api.authority, "POST", &connection_id, &payload).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: PostToConnection must answer 200, got {} {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        // A text frame carries UTF-8, so bytes that are not valid UTF-8 arrive with each
        // invalid sequence replaced — which is what the client reads back.
        let expected = String::from_utf8_lossy(&payload).to_string();
        match next_frame(&case, &mut socket).await {
            Message::Text(text) => assert_eq!(
                text.as_str(),
                expected,
                "case {case:?}: expected the delivered payload as a text frame, got {:?}",
                text.as_str()
            ),
            other => panic!("case {case:?}: expected a text frame, got {other:?}"),
        }
    }

    // One byte past the maximum is refused by the endpoint itself: the status alone, with
    // nothing naming an error shape, and nothing delivered.
    let answer = connections_call(
        case,
        &api.authority,
        "POST",
        &connection_id,
        &vec![b'x'; MAX_DATA_BYTES + 1],
    )
    .await;
    assert_eq!(
        answer.status, 413,
        "case {case:?}: a payload past the maximum must answer 413, got {} {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.error_type, "<absent>",
        "case {case:?}: the refusal names no error shape, got {:?}",
        answer.error_type
    );
    assert!(
        answer.body.is_empty(),
        "case {case:?}: the refusal carries no body, got {:?}",
        String::from_utf8_lossy(&answer.body)
    );

    // DeleteConnection closes the client's socket, with the normal closure a client library
    // reports rather than an abnormal one.
    let answer = connections_call(case, &api.authority, "DELETE", &connection_id, b"").await;
    assert_eq!(
        answer.status, 204,
        "case {case:?}: DeleteConnection must answer the modelled 204, got {} {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    match next_frame(case, &mut socket).await {
        Message::Close(Some(frame)) => assert_eq!(
            u16::from(frame.code),
            1000,
            "case {case:?}: the delete must close the socket normally, got {:?}",
            frame
        ),
        other => panic!("case {case:?}: expected a close frame, got {other:?}"),
    }

    // And from then on the id names a connection that no longer exists.
    for method in ["GET", "POST", "DELETE"] {
        let case = format!("{case}: {method} after the delete");
        let payload: &[u8] = if method == "POST" { b"x" } else { b"" };
        let answer =
            connections_call(&case, &api.authority, method, &connection_id, payload).await;
        assert_eq!(
            answer.status, 410,
            "case {case:?}: expected HTTP 410, got {} {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_type, "GoneException",
            "case {case:?}: expected GoneException, got {:?}",
            answer.error_type
        );
    }
}

/// A connection is addressed at the endpoint it was established at: one api's live id is
/// `GoneException` on another api's socket, and served on its own.
#[tokio::test]
async fn another_apis_live_connection_is_gone_on_this_apis_socket() {
    let case = "two apis, one live connection each";
    let emulator = emulator_with_connections();
    let first = deploy(&emulator, case, "first-api").await;
    let second = deploy(&emulator, case, "second-api").await;
    let (_first_socket, first_connection) = open_connection(case, &first.authority).await;
    let (_second_socket, second_connection) = open_connection(case, &second.authority).await;
    assert_ne!(
        first_connection, second_connection,
        "case {case:?}: each handshake must mint its own ConnectionId"
    );

    // (the socket the call is made on, the connection it names, whether that socket serves
    // it)
    let rows: [(&str, &str, &str, bool); 4] = [
        (
            "the first api's own connection",
            &first.authority,
            &first_connection,
            true,
        ),
        (
            "the second api's connection on the first api's socket",
            &first.authority,
            &second_connection,
            false,
        ),
        (
            "the second api's own connection",
            &second.authority,
            &second_connection,
            true,
        ),
        (
            "the first api's connection on the second api's socket",
            &second.authority,
            &first_connection,
            false,
        ),
    ];

    for (row, authority, connection_id, served) in rows {
        let answer = connections_call(row, authority, "GET", connection_id, b"").await;
        if served {
            assert_eq!(
                answer.status, 200,
                "case {row:?}: the endpoint serves this connection, so GetConnection must \
                 answer 200, got {} {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
        } else {
            assert_eq!(
                answer.status, 410,
                "case {row:?}: another api's connection must answer 410, got {} {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_type, "GoneException",
                "case {row:?}: expected GoneException, got {:?}",
                answer.error_type
            );
        }
    }
}

/// The `Sec-WebSocket-Key` the handshakes below offer: the 16-byte nonce RFC 6455 §1.3 writes
/// its own example with. The client library derives nothing from it but the accept value it
/// checks, so one value serves every case.
const CLIENT_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

/// The status a completed opening handshake is answered with (RFC 6455 §4.1).
const SWITCHING_PROTOCOLS: u16 = 101;

/// The message the `$default` route answers with the connection's own id, which is how a
/// client learns the id its `@connections` calls address.
const WHOAMI: &str = r#"{"action":"whoami"}"#;

/// How long the activity case keeps pushing before it gives up on `lastActiveAt` moving:
/// generous, because what it waits for is the host clock reading a later instant than the one
/// already stored, and a loaded machine schedules each push late. The wait is for that
/// CONDITION — the reported instant having moved — and nothing asserts on elapsed time.
const ADVANCE_DEADLINE: Duration = Duration::from_secs(120);

/// Open a connection on the api's socket whose opening handshake names `user_agent` — or names
/// none at all — and answer the socket and the `ConnectionId` the handshake minted.
///
/// The RFC 6455 §4.1 fields are written field by field rather than left to the client library,
/// because the field these rows differ along is one that library sends nothing for: a
/// `User-Agent` is the client's own, and a handshake that names none has to carry none.
async fn open_connection_naming(
    case: &str,
    authority: &str,
    user_agent: Option<&str>,
) -> (ClientSocket, String) {
    let stream = dial_loopback(case, authority)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the socket must accept a connection: {error}"));
    let mut builder = http::Request::builder()
        .uri(format!("ws://{authority}/{STAGE}"))
        .header("host", authority)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", CLIENT_KEY);
    if let Some(user_agent) = user_agent {
        builder = builder.header("user-agent", user_agent);
    }
    let request = builder
        .body(())
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake request must build: {error}"));
    let (mut socket, response) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must complete: {error}"));
    assert_eq!(
        response.status().as_u16(),
        SWITCHING_PROTOCOLS,
        "case {case:?}: a completed handshake must answer 101"
    );
    let connection_id = exchange_text(case, &mut socket, WHOAMI).await;
    assert!(
        !connection_id.is_empty(),
        "case {case:?}: the connection must have an id"
    );
    (socket, connection_id)
}

/// What one `GetConnection` report states about a live connection.
#[derive(Debug)]
struct Reported {
    /// `connectedAt`, as the (seconds, nanoseconds) pair its rendering decodes to, so two
    /// reports are comparable whatever subsecond digits each carried.
    connected_at: (i64, u32),
    /// `lastActiveAt`, read the same way.
    last_active_at: (i64, u32),
    /// The `identity` object as it arrived, or `Value::Null` for a report carrying none.
    identity: Value,
}

/// Read one connection's report off the api's OWN socket, as a backend's client reads it.
async fn reported(case: &str, authority: &str, connection_id: &str) -> Reported {
    let answer = connections_call(case, authority, "GET", &addressed(connection_id), b"").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetConnection must answer 200, got {} {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let value: Value = serde_json::from_slice(&answer.body)
        .unwrap_or_else(|error| panic!("case {case:?}: the report must be JSON: {error}"));
    Reported {
        connected_at: instant_of(case, &value, "connectedAt"),
        last_active_at: instant_of(case, &value, "lastActiveAt"),
        identity: value.get("identity").cloned().unwrap_or(Value::Null),
    }
}

/// The instant `member` states, parsed the way a client's own reader parses a modelled
/// `date-time`.
fn instant_of(case: &str, report: &Value, member: &str) -> (i64, u32) {
    let text = report
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the report must state {member}, got {report}"));
    let parsed = DateTime::from_str(text, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} must be an ISO 8601 date-time, got {text:?}: {error}")
    });
    (parsed.secs(), parsed.subsec_nanos())
}

/// A live connection's report is its OWN handshake's record: the address its client dialled
/// from, the `User-Agent` that handshake named itself with — and no `userAgent` member at all
/// when it named none — and an activity instant that is never before the instant the
/// connection was established.
#[tokio::test]
async fn a_live_connections_report_states_its_own_handshakes_identity() {
    // (case, the `User-Agent` the handshake carries, the `userAgent` the report must state)
    //
    // The rows differ along what the rule generalises over: whether the handshake named a user
    // agent at all, and the FORM of the field it named itself with — a client library's token
    // and version, a browser-shaped field carrying spaces and parentheses, a command-line
    // client's, and the EMPTY field, which a client that sent one is reported as having sent
    // rather than as having sent nothing.
    let rows: [(&str, Option<&str>, Option<&str>); 5] = [
        (
            "a client library naming itself with a version",
            Some("wscat/5.2.0"),
            Some("wscat/5.2.0"),
        ),
        (
            "a browser-shaped field with spaces and parentheses",
            Some("Mozilla/5.0 (compatible; emulator probe)"),
            Some("Mozilla/5.0 (compatible; emulator probe)"),
        ),
        (
            "a command-line client",
            Some("curl/8.5.0"),
            Some("curl/8.5.0"),
        ),
        ("an empty field", Some(""), Some("")),
        ("a handshake naming no user agent at all", None, None),
    ];

    let emulator = emulator_with_connections();
    let api = deploy(&emulator, "the identity table", "identity-api").await;
    // The address every client below dials, which is what a report's `sourceIp` states: read
    // off the announced socket rather than spelled out here.
    let (dialled, _port) = api.authority.rsplit_once(':').unwrap_or_else(|| {
        panic!(
            "setup: the announced socket must be a host:port, got {:?}",
            api.authority
        )
    });

    for (case, named, expected) in rows {
        // Held for the whole case: the report is of a connection that is still open.
        let (_socket, connection_id) = open_connection_naming(case, &api.authority, named).await;
        let stated = reported(case, &api.authority, &connection_id).await;

        let source_ip = stated.identity.get("sourceIp").and_then(Value::as_str);
        assert_eq!(
            source_ip,
            Some(dialled),
            "case {case:?}: expected identity.sourceIp {dialled:?}, got {source_ip:?} in {}",
            stated.identity
        );
        let user_agent = stated.identity.get("userAgent").and_then(Value::as_str);
        assert_eq!(
            user_agent, expected,
            "case {case:?}: expected identity.userAgent {expected:?}, got {user_agent:?} in {}",
            stated.identity
        );
        assert_eq!(
            stated.identity.get("userAgent").is_some(),
            expected.is_some(),
            "case {case:?}: expected the userAgent member present={}, got the identity {}",
            expected.is_some(),
            stated.identity
        );
        assert!(
            stated.last_active_at >= stated.connected_at,
            "case {case:?}: lastActiveAt {:?} must not be before connectedAt {:?}",
            stated.last_active_at,
            stated.connected_at
        );
    }
}

/// A REPORT leaves the connection exactly as the next caller finds it, and a DELIVERY is what
/// moves its activity: reading a connection never advances `lastActiveAt`, a payload delivered
/// over it does, and neither ever moves `connectedAt`.
#[tokio::test]
async fn a_report_leaves_last_active_at_where_a_delivery_moves_it() {
    let case = "the activity a live connection reports";
    let emulator = emulator_with_connections();
    let api = deploy(&emulator, case, "activity-api").await;
    let (mut socket, connection_id) =
        open_connection_naming(case, &api.authority, Some("activity/1")).await;

    let baseline = reported(case, &api.authority, &connection_id).await;
    assert!(
        baseline.last_active_at >= baseline.connected_at,
        "case {case:?}: lastActiveAt {:?} must not be before connectedAt {:?}",
        baseline.last_active_at,
        baseline.connected_at
    );

    // Reading it again changes nothing a later caller sees.
    for pass in ["the second read", "the third read"] {
        let again = reported(case, &api.authority, &connection_id).await;
        assert_eq!(
            again.connected_at, baseline.connected_at,
            "case {case:?}: {pass} must state the same connectedAt, got {:?} after {:?}",
            again.connected_at, baseline.connected_at
        );
        assert_eq!(
            again.last_active_at, baseline.last_active_at,
            "case {case:?}: {pass} must leave lastActiveAt where it was, got {:?} after {:?}",
            again.last_active_at, baseline.last_active_at
        );
    }

    // A payload delivered over the connection is what moves it. The push is repeated until the
    // reported instant HAS moved, because how many pushes that takes is the host clock's
    // resolution to decide, not the emulator's: the loop waits for the condition and gives up
    // only at [`ADVANCE_DEADLINE`].
    let deadline = Instant::now() + ADVANCE_DEADLINE;
    let mut advanced: Option<(i64, u32)> = None;
    while advanced.is_none() && Instant::now() < deadline {
        let answer =
            connections_call(case, &api.authority, "POST", &addressed(&connection_id), b"tick")
                .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: PostToConnection must answer 200, got {} {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        match next_frame(case, &mut socket).await {
            Message::Text(text) => assert_eq!(
                text.as_str(),
                "tick",
                "case {case:?}: expected the delivered payload, got {:?}",
                text.as_str()
            ),
            other => panic!("case {case:?}: expected a text frame, got {other:?}"),
        }
        let after = reported(case, &api.authority, &connection_id).await;
        assert_eq!(
            after.connected_at, baseline.connected_at,
            "case {case:?}: a delivery must not move connectedAt, got {:?} after {:?}",
            after.connected_at, baseline.connected_at
        );
        assert!(
            after.last_active_at >= baseline.last_active_at,
            "case {case:?}: lastActiveAt must never go backwards, got {:?} after {:?}",
            after.last_active_at,
            baseline.last_active_at
        );
        if after.last_active_at > baseline.last_active_at {
            advanced = Some(after.last_active_at);
        }
    }
    let advanced = advanced.unwrap_or_else(|| {
        panic!(
            "case {case:?}: a delivered payload must advance lastActiveAt past {:?}",
            baseline.last_active_at
        )
    });
    assert!(
        advanced > baseline.connected_at,
        "case {case:?}: the advanced lastActiveAt {advanced:?} must be after connectedAt {:?}",
        baseline.connected_at
    );
}
