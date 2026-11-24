// SPDX-License-Identifier: CC0-1.0

//! That a `@connections` call made on a deployed WEBSOCKET api's OWN endpoint always comes
//! BACK, whatever `ConnectionId` it names — read off the wire, under a deadline.
//!
//! > **RULE: the `@connections` route of a deployed stage is total: every request made on it
//! > is answered with one complete, modelled HTTP response, decided from the `ConnectionId`
//! > the request carries. A value API Gateway's connection-id encoding could never have
//! > produced is not a closed connection — it is refused from the value alone, before any
//! > connection is looked up, with HTTP 400 `BadRequestException` whose message is
//! > `Invalid connectionId: ` followed by the id the request sent; a value of that encoding
//! > naming no connection the endpoint holds is the memberless `GoneException` (HTTP 410);
//! > and a value naming a connection it holds is served. No `ConnectionId` leaves the request
//! > unanswered.**
//!
//! WHY the TERMINATION is a requirement of its own, and not covered by asserting the pair: a
//! backend reaches this endpoint from inside a handler that has an invoke deadline of its own.
//! A call that is answered `400` is one it branches on immediately — it keeps the row it merely
//! mis-spelled, and goes on serving the rest of its work — while a call that is never answered
//! blocks that handler until its own timeout and leaves it nothing to read at all, which an SDK
//! classifies as a retryable transport failure rather than as a verdict on the request. So each
//! case here reads the ANSWER off the socket under a generous deadline and fails naming what
//! arrived, rather than waiting on a response that may never come.
//!
//! What this suite adds to its siblings: `websocket_endpoint_connections.rs` owns the table of
//! statuses and bodies the endpoint answers a direct caller, and the management API crate's
//! own `connection_id_validity.rs` owns the two refusals as the router resolves them. Neither
//! reads its answer under a deadline, and neither drives an id whose characters a URI cannot
//! carry unencoded. Here the rows span what the rule generalises over: the FORM of the id (of
//! the issued encoding, and — refused by the alphabet, the padding, the decoded length, the
//! format marker, the length extremes and the characters a path segment has to encode — nine
//! that could not have been issued), the OPERATION (all three bind the same route), and the
//! endpoint's own state (nothing open, and a live connection open beside the refused call).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

use crate::websocket_endpoint_connections::{
    deploy, emulator_with_connections, minted, next_frame, open_connection, STAGE,
};
use crate::websocket_transport_data_plane::dial_loopback;

/// How long a case waits for the answer to one `@connections` call before failing: generous,
/// because these suites run beside every other test of the workspace and the answer may be
/// scheduled late on a host under that much contention, and bounded, because a route that
/// never answers is exactly what this suite exists to catch. What is waited for is the
/// ANSWER — a condition — and nothing here asserts on the clock.
const ANSWER_DEADLINE: Duration = Duration::from_secs(120);

/// The status and the error shape each class of `ConnectionId` is answered with: the modelled
/// `BadRequestException` (`@httpError(400)`) for a value the encoding could not have produced,
/// and the memberless `GoneException` (`@httpError(410)`) for one of that encoding naming no
/// connection the endpoint holds.
const INVALID: (u16, &str) = (400, "BadRequestException");
const GONE: (u16, &str) = (410, "GoneException");

/// What `GoneException` carries in its body: nothing. The shape declares no members, so the
/// answer names no message at all.
const NO_MESSAGE: &str = "<no message>";

/// The documented example `ConnectionId` — the id API Gateway's own `@connections`
/// documentation addresses — which is of the encoding this endpoint issues and names no
/// connection here.
const DOCUMENTED_ID: &str = "L0SM9cOFyjUCIhA=";

/// What one call was answered, as a client reads it off the wire.
struct Answered {
    status: u16,
    /// The `x-amzn-errortype` field, or `"<absent>"`: where a restJson1 client resolves an
    /// error SHAPE from.
    error_type: String,
    /// The `message` the body carries, or [`NO_MESSAGE`].
    message: String,
}

/// Make one `@connections` call on the api's own socket and read its answer under
/// [`ANSWER_DEADLINE`], the way a backend's SDK client makes one: the documented
/// `…/{stage}/@connections/{id}` path, signed for the data plane's own service, spoken as the
/// HTTP/1.1 it is.
///
/// `addressed` is the id as the REQUEST carries it (percent-encoded where a path segment has
/// to be, by `crate::path_label`), so a case can drive a value whose characters a URI cannot
/// carry unencoded.
async fn answered(
    case: &str,
    authority: &str,
    method: &str,
    addressed: &str,
    body: &[u8],
) -> Answered {
    let mut stream = dial_loopback(case, authority).await.unwrap_or_else(|error| {
        panic!("case {case:?}: the endpoint must accept a connection: {error}")
    });
    let head = format!(
        "{method} /{STAGE}/@connections/{addressed} HTTP/1.1\r\n\
         host: {authority}\r\n\
         authorization: AWS4-HMAC-SHA256 \
         Credential=AKIDTESTAPIGWV2/20261115/us-east-1/execute-api/aws4_request, \
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

    let mut read = Vec::new();
    // The ANSWER is what is awaited: the read ends when the endpoint has written its response
    // and closed, and a route that writes nothing fails the case here instead of holding it.
    match tokio::time::timeout(ANSWER_DEADLINE, stream.read_to_end(&mut read)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => panic!("case {case:?}: the answer must be readable, got {error}"),
        Err(_elapsed) => panic!(
            "case {case:?}: the endpoint must answer every @connections call; nothing \
             complete arrived, got {:?}",
            String::from_utf8_lossy(&read)
        ),
    }
    parsed(case, &read)
}

/// The answer those bytes state. A reply with no complete head is the failure this suite
/// exists to catch, so it panics naming what arrived.
fn parsed(case: &str, read: &[u8]) -> Answered {
    let split = read
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected one complete HTTP response, got {:?}",
                String::from_utf8_lossy(read)
            )
        });
    let head = String::from_utf8_lossy(&read[..split]).to_string();
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
    let body: serde_json::Value = serde_json::from_slice(read.get(split + 4..).unwrap_or_default())
        .unwrap_or(serde_json::Value::Null);
    let message = body
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(NO_MESSAGE)
        .to_string();
    Answered {
        status,
        error_type,
        message,
    }
}

/// One row: the form, the `ConnectionId` as a caller holds it, and what the endpoint owes it.
struct Row {
    form: &'static str,
    connection_id: String,
    /// The status and error shape, and the message the answer carries.
    expected: (u16, &'static str),
    expected_message: String,
}

impl Row {
    /// A value of the encoding API Gateway ISSUES that names no connection this endpoint
    /// holds: the memberless gone class.
    fn gone(form: &'static str, connection_id: String) -> Self {
        Self {
            form,
            connection_id,
            expected: GONE,
            expected_message: NO_MESSAGE.to_string(),
        }
    }

    /// A value that encoding could never have produced: refused from the value alone, with the
    /// sentence the management API words that refusal in — read from that crate's own
    /// constant, so the two cannot drift.
    fn invalid(form: &'static str, connection_id: &str) -> Self {
        Self {
            form,
            connection_id: connection_id.to_string(),
            expected: INVALID,
            expected_message: mgmt::state::invalid_connection_id_message(connection_id),
        }
    }
}

/// Every `ConnectionId` form a caller may send, and the answer it is owed. The rows span the
/// ways a value fails the encoding — the alphabet, the padding, the decoded length (too few
/// bytes, too many), the format marker, the length extremes, and characters a path segment
/// has to percent-encode — beside the forms the encoding does produce.
fn rows() -> Vec<Row> {
    vec![
        Row::gone(
            "an id of the encoding this endpoint issues, naming no connection",
            minted("terminal-one"),
        ),
        Row::gone("another id of that encoding", minted("terminal-two")),
        Row::gone(
            "the id API Gateway's own documentation addresses",
            DOCUMENTED_ID.to_string(),
        ),
        Row::invalid("a single character", "x"),
        Row::invalid("text that is not base64", "not base64!!"),
        Row::invalid("an unpadded token", "Fabc1cWIPHcCJ7A"),
        Row::invalid("a dashed token", "cid-with-dashes"),
        Row::invalid("a token of base64url characters", "a-b_c9DEF0123456"),
        Row::invalid("base64 of too few bytes", "jR4tQ=="),
        Row::invalid("base64 of too many bytes", "0123456789abcdef"),
        Row::invalid(
            "base64 of eleven zero bytes, so no format marker",
            "AAAAAAAAAAAAAAA=",
        ),
        Row::invalid("a token carrying a path separator", "cid/with/separators"),
        Row::invalid("a token carrying a query delimiter", "cid?with=query"),
        Row::invalid("a token carrying non-ASCII text", "connexión-identifiant"),
        Row::invalid("a 200-character token", &"A".repeat(200)),
    ]
}

/// Every `@connections` call on a deployed stage's own endpoint is ANSWERED, and the answer is
/// the class the `ConnectionId` earns: no id of any form leaves the call hanging.
#[tokio::test]
async fn every_connections_call_is_answered_whatever_connection_id_it_names() {
    let emulator = emulator_with_connections();
    let api = deploy(&emulator, "the terminal-answer table", "terminal-api").await;

    for row in rows() {
        let case = format!("POST on {} ({})", row.form, row.connection_id);
        let addressed = crate::path_label::addressed(&row.connection_id);
        let answer = answered(&case, &api.authority, "POST", &addressed, b"push").await;
        let (expected_status, expected_type) = row.expected;
        assert_eq!(
            answer.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {} {:?}",
            answer.status, answer.message
        );
        assert_eq!(
            answer.error_type, expected_type,
            "case {case:?}: expected x-amzn-errortype {expected_type:?}, got {:?}",
            answer.error_type
        );
        assert_eq!(
            answer.message, row.expected_message,
            "case {case:?}: expected the message {:?}, got {:?}",
            row.expected_message, answer.message
        );
    }
}

/// The same totality for the two READING operations bound to that route: all three bind
/// `/@connections/{ConnectionId}`, so none of them may leave a call unanswered either.
#[tokio::test]
async fn the_reading_operations_of_that_route_are_answered_too() {
    let emulator = emulator_with_connections();
    let api = deploy(&emulator, "the terminal-answer methods", "terminal-methods-api").await;

    for row in rows() {
        for method in ["GET", "DELETE"] {
            let case = format!("{method} on {} ({})", row.form, row.connection_id);
            let addressed = crate::path_label::addressed(&row.connection_id);
            let answer = answered(&case, &api.authority, method, &addressed, b"").await;
            let (expected_status, expected_type) = row.expected;
            assert_eq!(
                answer.status, expected_status,
                "case {case:?}: expected HTTP {expected_status}, got {} {:?}",
                answer.status, answer.message
            );
            assert_eq!(
                answer.error_type, expected_type,
                "case {case:?}: expected x-amzn-errortype {expected_type:?}, got {:?}",
                answer.error_type
            );
        }
    }
}

/// A refused call decides that call and nothing else: with a client connected, a push to a
/// malformed id is refused `400` and the LIVE connection is still posted to, the payload
/// arriving on its own socket. So a backend that mis-spelled one id keeps serving the
/// connections it holds.
#[tokio::test]
async fn a_refused_call_leaves_the_live_connection_servable() {
    let case = "a refusal beside a live connection";
    let emulator = emulator_with_connections();
    let api = deploy(&emulator, case, "terminal-live-api").await;
    let (mut socket, connection_id) = open_connection(case, &api.authority).await;

    // Three refused calls in a row, one per class of malformed value, each answered on its
    // own and none of them touching the open connection.
    for malformed in ["x", "cid/with/separators", &"A".repeat(200)] {
        let case = format!("{case}: a push to {malformed:?}");
        let addressed = crate::path_label::addressed(malformed);
        let answer = answered(&case, &api.authority, "POST", &addressed, b"push").await;
        assert_eq!(
            answer.status,
            INVALID.0,
            "case {case:?}: expected HTTP {}, got {} {:?}",
            INVALID.0,
            answer.status,
            answer.message
        );
        assert_eq!(
            answer.message,
            mgmt::state::invalid_connection_id_message(malformed),
            "case {case:?}: expected the refusal to name the id the request sent, got {:?}",
            answer.message
        );
    }

    // And the connection the endpoint holds is served: the payload is delivered to that
    // client's own socket.
    let delivered = b"{\"notice\":\"still here\"}";
    let addressed = crate::path_label::addressed(&connection_id);
    let answer = answered(case, &api.authority, "POST", &addressed, delivered).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: a push to the live connection {connection_id:?} must answer 200, got \
         {} {:?}",
        answer.status, answer.message
    );
    let frame = next_frame(case, &mut socket).await;
    let read = match &frame {
        tokio_tungstenite::tungstenite::Message::Text(text) => text.as_bytes().to_vec(),
        tokio_tungstenite::tungstenite::Message::Binary(bytes) => bytes.to_vec(),
        other => panic!("case {case:?}: expected the delivered payload, got {other:?}"),
    };
    assert_eq!(
        read,
        delivered.to_vec(),
        "case {case:?}: expected the delivered payload {:?}, got {:?}",
        String::from_utf8_lossy(delivered),
        String::from_utf8_lossy(&read)
    );
}
