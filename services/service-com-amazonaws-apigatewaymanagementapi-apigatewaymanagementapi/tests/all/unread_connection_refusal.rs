// SPDX-License-Identifier: CC0-1.0

//! What `PostToConnection` answers for a connection whose client has STOPPED READING its
//! socket, driven over HTTP through the same `MultiServiceRouter` the production emulator
//! serves with.
//!
//! > **A delivery to a connection whose client side buffer is full is refused with the
//! > modelled `LimitExceededException` — `@httpError(429)` — carrying the sentence API Gateway
//! > words THAT condition in, and nothing is recorded as delivered; the connection stays
//! > open.** The modelled shape declares no members
//! > (`smithy_query(query="shape", shape="LimitExceededException")`), while the endpoint
//! > answers one of the two conditions the shape documents — "the client is sending more than
//! > the allowed number of requests per unit of time or the WebSocket client side buffer is
//! > full" — so the sentence says which, and a sender can tell a full buffer from a connection
//! > that ended (`GoneException`, which has it prune its own record).
//!
//! The rows differ along what the rule generalises over: the size of the payload the refused
//! delivery carried (one byte, a kilobyte, and the modelled maximum), and — as the control —
//! whether the client is reading at all, which is what decides refusal rather than how many
//! payloads were sent.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use engine::InMemoryEngine;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

use crate::support::{call_with_headers, error_code, error_message, minted, rendered, HOST};

/// The `Data` maximum the model declares, and the queue depth one connection holds, both read
/// from the crate's own constants rather than spelled out here.
const MAX_DATA_BYTES: usize = mgmt::ops::post_to_connection::MAX_DATA_BYTES;
const PUSH_DEPTH: usize = mgmt::state::CONNECTION_PUSH_DEPTH;

/// A registration instant in the past.
fn connected_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// The request path for `connection_id`.
fn path(connection_id: &str) -> String {
    format!("/@connections/{connection_id}")
}

/// One connection carried on a LIVE channel, with the receiving end handed back: holding it
/// without reading is a client that has stopped draining its socket, and dropping it is a
/// client whose connection ended.
fn live_connection(
    engine: &Arc<InMemoryEngine>,
    case: &str,
) -> (String, ::tokio::sync::mpsc::Receiver<Vec<u8>>) {
    let registry = mgmt::state::registry(&**engine).expect("the engine carries the registry");
    let connection_id = minted(case);
    let (pushes, pushed) = ::tokio::sync::mpsc::channel(PUSH_DEPTH);
    assert!(
        mgmt::state::open_connection(
            &registry,
            connection_id.clone(),
            mgmt::state::Identity {
                source_ip: "192.0.2.60".to_string(),
                user_agent: Some("unread/1".to_string()),
            },
            connected_at(),
            None,
            Some(pushes),
        ),
        "case {case:?}: the connection must be opened on a live channel"
    );
    (connection_id, pushed)
}

/// Deliver `payload` to `connection_id`, answering the status, the error code and the error
/// message of what came back.
async fn post(
    engine: &Arc<InMemoryEngine>,
    connection_id: &str,
    payload: Vec<u8>,
) -> (u16, String, String) {
    let (status, headers, body) =
        call_with_headers(engine, HOST, "POST", &path(connection_id), payload).await;
    let code = error_code(&headers);
    let message = if status == 200 {
        rendered(&body)
    } else {
        error_message(&body)
    };
    (status, code, message)
}

/// The sizes a refused delivery's payload is driven at: the smallest, a middling one, and the
/// modelled maximum — the refusal is about the client's buffer, not about the payload.
const REFUSED_PAYLOAD_BYTES: [(&str, usize); 3] = [
    ("one byte", 1),
    ("a kilobyte", 1024),
    ("the modelled maximum", MAX_DATA_BYTES),
];

#[tokio::test]
async fn a_delivery_to_a_client_that_stopped_reading_is_refused_with_the_modelled_429() {
    for (size_case, payload_bytes) in REFUSED_PAYLOAD_BYTES {
        let case = format!("a refused delivery of {size_case}");
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let (connection_id, unread) = live_connection(&engine, &case);

        // Fill the connection's queue: the client is holding its end and reading nothing, so
        // every payload stays waiting.
        for index in 0..PUSH_DEPTH {
            let (status, code, message) = post(&engine, &connection_id, b"x".to_vec()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: payload {index} must still be accepted, got {status} {code} \
                 {message}"
            );
        }

        let before = mgmt::state::connection(&*engine, &connection_id)
            .unwrap_or_else(|| panic!("case {case:?}: the filled connection must be open"));

        let (status, code, message) =
            post(&engine, &connection_id, vec![b'y'; payload_bytes]).await;
        assert_eq!(
            status, 429,
            "case {case:?}: expected the modelled 429, got {status} {code} {message}"
        );
        assert_eq!(
            code, "LimitExceededException",
            "case {case:?}: expected the modelled shape in x-amzn-errortype, got {code:?}"
        );
        assert_eq!(
            message, "Too Many Requests. WebSocket client buffer full",
            "case {case:?}: expected the sentence API Gateway words a full client buffer in, \
             got {message:?}"
        );

        // Nothing of the refused payload was recorded, and the connection is still open: a
        // full buffer is not a connection that ended.
        let stored = mgmt::state::connection(&*engine, &connection_id)
            .unwrap_or_else(|| panic!("case {case:?}: the connection must still be open"));
        assert_eq!(
            stored.delivered_count, PUSH_DEPTH as u64,
            "case {case:?}: expected {PUSH_DEPTH} recorded deliveries, got {}",
            stored.delivered_count
        );
        assert_eq!(
            stored.last_active_at, before.last_active_at,
            "case {case:?}: a refused delivery must not advance LastActiveAt"
        );

        // The client's end is released only here, so every call above was made against a
        // connection that was live and unread.
        drop(unread);
    }
}

/// The CONTROL: the same connection, with its client reading, takes more payloads than its
/// queue holds — so what refuses a delivery is the unread backlog and not the number of
/// deliveries made.
#[tokio::test]
async fn a_client_that_is_reading_is_served_past_the_queue_depth() {
    let case = "a draining client";
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let (connection_id, mut reading) = live_connection(&engine, case);

    let payloads = PUSH_DEPTH + 2;
    for index in 0..payloads {
        let payload = format!("frame-{index}").into_bytes();
        let (status, code, message) = post(&engine, &connection_id, payload.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: payload {index} must be delivered, got {status} {code} {message}"
        );
        let read = reading
            .recv()
            .await
            .unwrap_or_else(|| panic!("case {case:?}: payload {index} must reach the client"));
        assert_eq!(
            read, payload,
            "case {case:?}: the client must read payload {index} as it was sent"
        );
    }

    let stored = mgmt::state::connection(&*engine, &connection_id)
        .unwrap_or_else(|| panic!("case {case:?}: the connection must still be open"));
    assert_eq!(
        stored.delivered_count, payloads as u64,
        "case {case:?}: expected {payloads} recorded deliveries, got {}",
        stored.delivered_count
    );
}
