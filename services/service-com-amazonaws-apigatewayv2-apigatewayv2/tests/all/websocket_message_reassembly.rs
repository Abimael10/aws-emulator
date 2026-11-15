// SPDX-License-Identifier: CC0-1.0

//! What the INTEGRATION of a WEBSOCKET route sees of a client's message, and what the client
//! reads back of that integration's own answer, over a real connection.
//!
//! The rule both cases below are slices of:
//!
//! > A client's message on a deployed WEBSOCKET stage is dispatched to the route the api's
//! > `RouteSelectionExpression` selects as ONE message — exactly the payload the message's
//! > frames assemble to, whatever framing the client wrote it with (RFC 6455 §5.4) — and
//! > whatever that route's integration answers is delivered to the client as a TEXT frame
//! > carrying exactly those bytes: the reply of a two-way route, and the out-of-band
//! > `PostToConnection` the handler makes on the `@connections` endpoint its own event names.
//!
//! Nothing here is a value this emulator fills in: every assertion is on what the FUNCTION
//! reported about the body it was handed — its byte count and its SHA-256 digest, both computed
//! inside the run — so a message delivered a frame at a time, truncated at a frame boundary or
//! re-encoded is read as a different count or digest rather than as a frame that merely
//! arrived. That is why AWS Lambda and the `@connections` management API are mounted beside
//! this service on one engine: the answer belongs to a function that service runs, and the push
//! travels through the management API that owns connections.
//!
//! The suites that own the neighbouring rules are not restated here: the refusals of a message
//! this plane does NOT serve (a frame whose opcode is not text, a frame past the frame quota, a
//! message past the per-message ceiling) are `websocket_message_refusals.rs`, the event each
//! call of a connection is handed is `websocket_lambda_routes.rs`, and the fan-out of a push to
//! connections OTHER than the sender's is `websocket_call_context.rs`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use smithy_server_runtime::MultiServiceRouter;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::Message;

use crate::api_lifecycle::call_in_region;
use crate::http_api_backend_dispatch::given_function;
use crate::websocket_transport_data_plane::{handshake_at, ClientSocket};

/// The Region every request below is signed for — the one the Lambda harness creates its
/// functions in, so the api and the function its routes name live in the same Region.
const REGION: &str = "us-east-1";

/// The stage every case deploys.
const STAGE: &str = "prod";

/// The largest payload ONE frame of a client's message carries, in bytes: the 32 KiB API
/// Gateway publishes as a WebSocket api's frame quota, so a message larger than this arrives as
/// several frames and a row may write a frame exactly at it.
const FRAME_QUOTA_BYTES: usize = 32 * 1024;

/// The per-message ceiling a client's message is served under, in bytes: the 128 KiB API
/// Gateway publishes as a WebSocket api's message payload quota. A message AT it is served, so
/// the integration must see all of it.
const MESSAGE_CEILING_BYTES: usize = 131_072;

/// The route key of the TWO-WAY route, whose integration's reply is sent straight back, and the
/// `$default` route, which is one-way here so the only frame it produces is the one the function
/// PUSHED.
const REPLY_ROUTE_KEY: &str = "reply";
const DEFAULT_ROUTE_KEY: &str = "$default";

/// How long a case waits for the frame its message is answered with before failing: generous
/// for a loaded test machine — the answer is a function this emulator has to run first — and
/// bounded so a case that will never be answered fails instead of hanging the suite.
const FRAME_DEADLINE: Duration = Duration::from_secs(120);

/// This service, AWS Lambda and the `@connections` management API on ONE engine: a route's
/// function is run by that Lambda, and the push it makes travels through the management API that
/// owns the connection this api's socket carries.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One control-plane request that must answer `201`, signed by the one caller this crate's
/// suites share.
async fn created(
    emulator: &MultiServiceRouter,
    case: &str,
    what: &str,
    uri: &str,
    body: Value,
) -> Value {
    let (status, answered) = call_in_region(emulator, REGION, "POST", uri, Some(body)).await;
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

/// A handler that reports what it was HANDED: the byte count and the SHA-256 digest of the
/// body of its event, computed inside the run.
///
/// The two-way route's call is answered with that report as the response body, so the gateway
/// relays it; every other call PUSHES it to its own connection through the `@connections`
/// endpoint the event names, which is how a one-way route answers a client at all.
fn digesting_handler() -> String {
    "import hashlib\n\
     import json\n\
     import boto3\n\
     \n\
     \n\
     def lambda_handler(event, context):\n\
     \x20   body = (event.get(\"body\") or \"\").encode(\"utf-8\")\n\
     \x20   digest = hashlib.sha256(body).hexdigest()\n\
     \x20   report = json.dumps({\"len\": len(body), \"sha\": digest})\n\
     \x20   reported = event[\"requestContext\"]\n\
     \x20   if reported[\"routeKey\"] == \"reply\":\n\
     \x20       return {\"statusCode\": 200, \"body\": report}\n\
     \x20   connections = boto3.client(\n\
     \x20       \"apigatewaymanagementapi\",\n\
     \x20       endpoint_url=\"https://%s/%s\" % (reported[\"domainName\"], reported[\"stage\"]),\n\
     \x20   )\n\
     \x20   connections.post_to_connection(\n\
     \x20       ConnectionId=reported[\"connectionId\"], Data=report.encode(\"utf-8\")\n\
     \x20   )\n\
     \x20   return {\"statusCode\": 200}\n"
        .to_string()
}

/// A deployed WEBSOCKET api whose `$default` route is ONE-WAY and whose `reply` route is
/// two-way, both fronting `function` through an `AWS_PROXY` integration. Answers the `host:port`
/// its `ApiEndpoint` announces.
async fn given_socket(
    emulator: &MultiServiceRouter,
    case: &str,
    name: &str,
    function: &str,
) -> String {
    let api = created(
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
    let api_id = text(case, "CreateApi", &api, "apiId");
    let announced = text(case, "CreateApi", &api, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
        })
        .to_string();

    for (route_key, two_way) in [(DEFAULT_ROUTE_KEY, false), (REPLY_ROUTE_KEY, true)] {
        let integration = created(
            emulator,
            case,
            "CreateIntegration",
            &format!("/v2/apis/{api_id}/integrations"),
            json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": function,
                "integrationMethod": "POST"
            }),
        )
        .await;
        let integration_id = text(case, "CreateIntegration", &integration, "integrationId");
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
    created(
        emulator,
        case,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": STAGE }),
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
    authority
}

/// `count` bytes of single-byte text, as one message of that size.
fn ascii_of(count: usize) -> String {
    "a".repeat(count)
}

/// `repeats` copies of a unit carrying two- and three-byte characters, so a frame boundary
/// inside one is a boundary inside a CHARACTER — which RFC 6455 §5.4 permits, since a fragment
/// is a byte range and only the assembled message is text.
fn multibyte_of(repeats: usize) -> String {
    "héllo✓".repeat(repeats)
}

/// `message` written as the frames `sizes` names, in bytes: the first carries the text opcode
/// and is not final, every later one continues it, and the last ends the message (RFC 6455
/// §5.4). A single size is therefore one ordinary text frame.
fn written_as(case: &str, message: &str, sizes: &[usize]) -> Vec<Message> {
    let total: usize = sizes.iter().sum();
    assert_eq!(
        total,
        message.len(),
        "case {case:?}: the row's frames must carry the whole message of {} bytes, got {total}",
        message.len()
    );
    assert!(
        !sizes.is_empty(),
        "case {case:?}: a message is written as at least one frame"
    );
    let payload = message.as_bytes();
    let last = sizes.len() - 1;
    let mut written = Vec::with_capacity(sizes.len());
    let mut at = 0usize;
    for (index, size) in sizes.iter().enumerate() {
        let chunk = &payload[at..at + size];
        at += size;
        let opcode = match index {
            0 => OpCode::Data(Data::Text),
            _ => OpCode::Data(Data::Continue),
        };
        written.push(Message::Frame(Frame::message(
            Bytes::copy_from_slice(chunk),
            opcode,
            index == last,
        )));
    }
    written
}

/// The SHA-256 digest of `message`'s bytes, as the hexadecimal the handler reports.
fn digest_of(message: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(message.as_bytes());
    hex::encode(hasher.finalize())
}

/// Write `frames` as they stand.
async fn write_frames(case: &str, socket: &mut ClientSocket, frames: Vec<Message>) {
    for frame in frames {
        socket
            .send(frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: each frame must be sent, got {error}"));
    }
}

/// The next TEXT frame this socket is sent, read as the function's own report.
async fn reported(case: &str, what: &str, socket: &mut ClientSocket) -> Value {
    let frame = match tokio::time::timeout(FRAME_DEADLINE, socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text.as_str().to_string(),
        Ok(Some(Ok(other))) => {
            panic!("case {case:?}: {what} must arrive as a text frame, got {other:?}")
        }
        Ok(Some(Err(error))) => panic!("case {case:?}: the socket failed: {error}"),
        Ok(None) => {
            panic!("case {case:?}: the socket ended instead of carrying {what}")
        }
        Err(_elapsed) => panic!(
            "case {case:?}: {what} did not arrive within {FRAME_DEADLINE:?}, so the \
             integration's answer never reached the client"
        ),
    };
    serde_json::from_str(&frame).unwrap_or_else(|error| {
        panic!("case {case:?}: {what} must be the function's own report, got {frame:?} ({error})")
    })
}

/// Assert `answer` is the report the function computes for `message`.
fn assert_reports(case: &str, what: &str, message: &str, answer: &Value) {
    let length = answer.get("len").and_then(Value::as_u64);
    assert_eq!(
        length,
        Some(message.len() as u64),
        "case {case:?}: {what} must report the whole assembled message of {} bytes, got \
         {length:?} ({answer})",
        message.len()
    );
    let reported_digest = answer.get("sha").and_then(Value::as_str);
    let expected = digest_of(message);
    assert_eq!(
        reported_digest,
        Some(expected.as_str()),
        "case {case:?}: {what} must report the digest of the whole message, expected \
         {expected:?}, got {reported_digest:?} ({answer})"
    );
}

/// A client's message reaches the serving route's integration as ONE message — the payload its
/// frames assemble to — whatever framing the client wrote it with, and the push that integration
/// makes carries exactly those bytes back to the client.
#[tokio::test]
async fn a_fragmented_client_message_reaches_the_integration_as_one_message() {
    /// One row: the message the client sends, and the byte lengths of the frames it writes it
    /// as.
    struct Row {
        case: &'static str,
        message: String,
        frames: Vec<usize>,
    }

    // The rows span what the rule generalises over: the number of frames one message is written
    // as (one, two, many), the sizes of those frames (equal, uneven, empty, one exactly at the
    // published frame quota), the size of the assembled message (a few bytes up to the published
    // per-message ceiling), and whether a frame boundary falls inside a multi-byte character.
    let rows = vec![
        Row {
            case: "one text frame",
            message: ascii_of(64),
            frames: vec![64],
        },
        Row {
            case: "two fragments of unequal size",
            message: ascii_of(16),
            frames: vec![5, 11],
        },
        Row {
            case: "seven fragments of 16 KiB each",
            message: ascii_of(7 * 16 * 1024),
            frames: vec![16 * 1024; 7],
        },
        Row {
            case: "uneven fragments, one of them exactly at the frame quota",
            message: ascii_of(1 + FRAME_QUOTA_BYTES + 7 + 12_345),
            frames: vec![1, FRAME_QUOTA_BYTES, 7, 12_345],
        },
        Row {
            case: "a fragmented message carrying an empty continuation frame",
            message: ascii_of(7),
            frames: vec![3, 0, 4],
        },
        Row {
            case: "fragments assembling to exactly the per-message ceiling",
            message: ascii_of(MESSAGE_CEILING_BYTES),
            frames: vec![FRAME_QUOTA_BYTES; MESSAGE_CEILING_BYTES / FRAME_QUOTA_BYTES],
        },
        Row {
            // 5 copies of a 9-byte unit, cut at byte 2 — inside the two-byte `é` of the first
            // copy — so a gateway that read each frame as its own text message could not even
            // decode it.
            case: "fragments split inside a multi-byte character",
            message: multibyte_of(5),
            frames: vec![2, 20, 23],
        },
    ];

    let suite = "a fragmented client message";
    let (emulator, engine) = emulator();
    let function =
        given_function(&emulator, &engine, suite, "socket-digest", &digesting_handler()).await;
    let authority = given_socket(&emulator, suite, "reassembly-api", &function).await;
    // One connection serves every row: no row is refused, so the socket stays open and each
    // row's push is the next frame read on it.
    let mut socket: ClientSocket = handshake_at(suite, &authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {suite:?}: the handshake must be completed, got {status}")
        });

    for row in rows {
        let case = row.case;
        let frames = written_as(case, &row.message, &row.frames);
        write_frames(case, &mut socket, frames).await;
        let answer = reported(case, "the integration's push", &mut socket).await;
        assert_reports(case, "the integration's push", &row.message, &answer);
    }
}

/// The reply of a TWO-WAY route is the integration's own bytes, for a message the client wrote
/// as fragments too: the route response path and the push path carry the same answer.
#[tokio::test]
async fn a_two_way_routes_reply_carries_the_integrations_own_bytes() {
    let case = "a two-way route answering a fragmented message";
    let (emulator, engine) = emulator();
    let function =
        given_function(&emulator, &engine, case, "socket-reply", &digesting_handler()).await;
    let authority = given_socket(&emulator, case, "reassembly-reply-api", &function).await;
    let mut socket: ClientSocket = handshake_at(case, &authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: the handshake must be completed, got {status}")
        });

    // The selection expression reads the message's `action`, so the whole assembled message is
    // what names the route: a gateway dispatching the first fragment alone would route it
    // nowhere.
    let message = json!({
        "action": REPLY_ROUTE_KEY,
        "payload": ascii_of(3 * FRAME_QUOTA_BYTES),
    })
    .to_string();
    // The first frame is the few bytes that name the route; the rest follows in frames of at
    // most the published quota, so nothing here is refused for its own size.
    let head = 11;
    let mut frames = vec![head];
    let mut left = message.len() - head;
    while left > 0 {
        let take = left.min(FRAME_QUOTA_BYTES);
        frames.push(take);
        left -= take;
    }
    let written = written_as(case, &message, &frames);
    write_frames(case, &mut socket, written).await;
    let answer = reported(case, "the two-way route's reply", &mut socket).await;
    assert_reports(case, "the two-way route's reply", &message, &answer);
}
