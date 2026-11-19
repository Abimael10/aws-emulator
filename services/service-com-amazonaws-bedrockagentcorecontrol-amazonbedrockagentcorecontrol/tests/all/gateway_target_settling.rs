// SPDX-License-Identifier: CC0-1.0

//! The ONE reading every gateway-target suite of this crate waits for a target's
//! clock-owed transition by: which `TargetStatus` values a target SETTLES into, and the
//! poll that reads a target again until it reports one of them.
//!
//! One copy, so four suites cannot drift apart over which statuses are terminal — a
//! target's status settles into what its own tool-definition fetch achieved
//! (`READY` or `FAILED`), so a suite that waited for one spelling of "settled" would
//! hang on the other. The wait is for the CONDITION, never for a fixed time: a host
//! slower than this one only reads again.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// The `TargetStatus` values a target SETTLES into: the transient ones the clock owes a
/// transition out of are every other value of the enum
/// (`smithy_query(query="shape",shape="TargetStatus")`).
pub const TERMINAL_STATUSES: &[&str] = &[
    "READY",
    "FAILED",
    "UPDATE_UNSUCCESSFUL",
    "SYNCHRONIZE_UNSUCCESSFUL",
];

/// How long a wait for a clock-owed transition lasts at the most, and how long it pauses
/// between two reads: generous, because what is being waited for is the condition and not
/// a duration under test.
pub const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
pub const POLL_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// Whether `status` is one a target settles into.
#[must_use]
pub fn is_terminal(status: &str) -> bool {
    TERMINAL_STATUSES.contains(&status)
}

/// Read `gateway`'s `target` until the clock has made its transition out of the transient
/// status due, and hand back that answer as `(status, body)`.
///
/// The read is a `GetGatewayTarget` signed the way the router routes one, so a suite polls
/// through the same wire its own rows drive.
pub async fn settled_target(
    router: &MultiServiceRouter,
    gateway: &str,
    target: &str,
) -> (u16, Value) {
    let started = std::time::Instant::now();
    loop {
        let read = get_target(router, gateway, target).await;
        let status = read.1.get("status").and_then(Value::as_str).unwrap_or_default();
        if is_terminal(status) || started.elapsed() > POLL_DEADLINE {
            return read;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Read `gateway`'s `target` until the removal an accepted deletion owes it has come due,
/// and hand back that answer as `(status, body)`.
pub async fn removed_target(
    router: &MultiServiceRouter,
    gateway: &str,
    target: &str,
) -> (u16, Value) {
    let started = std::time::Instant::now();
    loop {
        let read = get_target(router, gateway, target).await;
        if read.0 == 404 || started.elapsed() > POLL_DEADLINE {
            return read;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// One `GetGatewayTarget`, signed for the Region the suites sign for.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/us-east-1/\
     bedrock-agentcore/aws4_request, SignedHeaders=host, Signature=abc";

async fn get_target(router: &MultiServiceRouter, gateway: &str, target: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/gateways/{gateway}/targets/{target}/"))
        .header("authorization", AUTHORIZATION)
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com")
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, body)
}
