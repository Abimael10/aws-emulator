// SPDX-License-Identifier: CC0-1.0

//! The wire harness every suite of this test binary can share: one service mounted on
//! its own engine, reached through the same `MultiServiceRouter` the emulator boots, and
//! the few calls a query-execution test makes over it.
//!
//! The suites of `tests/all.rs` are modules of ONE binary, so a helper belongs here
//! rather than beside each test that needs it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{QUERY_STATE_QUEUED, QUERY_STATE_RUNNING};
use service_com_amazonaws_athena_amazonathena::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The `x-amz-target` prefix of this service: the local name of the Smithy service
/// shape (`com.amazonaws.athena#AmazonAthena`), which is how the awsJson protocols
/// build that header.
const TARGET_PREFIX: &str = "AmazonAthena";

/// The access key id every suite that does not care about the caller signs with: a key
/// neither STS nor IAM holds a record for, which is the position the account's own
/// published credential is in.
pub const ACCOUNT_ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// The router picks a service by the SigV4 credential scope's service field, so every
/// request carries one naming `athena` (this service's `sigv4Name`). Neither the key id
/// nor the signature is a credential: [`router`] turns request authentication off, so
/// nothing here is verified — the key id only names the principal a request speaks for.
fn authorization(access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential={access_key_id}/20260101/us-east-1/athena/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// One service mounted on its own engine, reached the way the production router reaches
/// it.
pub fn router() -> MultiServiceRouter {
    engine_and_router().1
}

/// The buckets the suites that share this harness write their query results into.
///
/// Athena verifies the resolved output location before it accepts a submission — one naming
/// a bucket the account does not hold is a location it can neither verify nor create — so
/// the mounting below HOLDS them, exactly as a client creates its results bucket before it
/// runs its first query. A suite with a results bucket of its own holds it itself
/// (`crate::results_bucket::hold_results_bucket`).
const RESULTS_BUCKETS: &[&str] = &["athena-results", "results", "somewhere-else", "later"];

/// The same mounting, handing back the ENGINE as well, for an assertion whose reader is
/// still a scaffold and that therefore has to be made against the store itself.
pub fn engine_and_router() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so a suite does not depend on how the emulator process was started:
    // these tests assert Athena's behaviour, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    for bucket in RESULTS_BUCKETS {
        crate::results_bucket::hold_bucket(&engine, bucket);
    }
    (engine, router)
}

/// Invoke one operation signed with the account's own published credential, and return
/// `(status, body)`.
pub async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call_as(router, ACCOUNT_ACCESS_KEY_ID, operation, body).await
}

/// Invoke one operation signed with `access_key_id` — what names the principal the request
/// speaks for — and return `(status, body)`.
pub async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, _headers, parsed) = call_reported(router, access_key_id, operation, body).await;
    (status, parsed)
}

/// The same ONE request builder, answering the response's HEADERS beside its status and
/// body — for a suite whose subject is something a response reports in a header rather than
/// in its document, as the request a response names itself with is
/// (`tests/all/response_request_id.rs`).
pub async fn call_reported(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, smithy_server_runtime::http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "athena.us-east-1.amazonaws.com")
        .header("authorization", authorization(access_key_id))
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("content-type", "application/x-amz-json-1.1")
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, headers, parsed)
}

/// Start one query and return the `QueryExecutionId` it minted.
pub async fn start(router: &MultiServiceRouter, body: &Value) -> String {
    let (status, answer) = call(router, "StartQueryExecution", body).await;
    assert_eq!(
        status, 200,
        "StartQueryExecution({body}) must be accepted, got {status} with {answer:?}"
    );
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartQueryExecution must answer with an id, got {answer:?}"))
        .to_string()
}

/// The stored `QueryExecution` of `id`.
pub async fn execution(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, body) = call(router, "GetQueryExecution", &json!({ "QueryExecutionId": id })).await;
    assert_eq!(
        status, 200,
        "GetQueryExecution({id}) must answer 200, got {status} with {body:?}"
    );
    body["QueryExecution"].clone()
}

/// Poll until the execution `id` has reached a terminal state, and return it.
///
/// The CONDITION is what is waited on, not a span: every look asks the service what state
/// the execution is in, and the deadline is far above the span a completion is due after,
/// so a loaded host cannot turn a slow run into a failure. Shared here because every suite
/// that reads a finished query needs it.
pub async fn settled(router: &MultiServiceRouter, id: &str) -> Value {
    for _ in 0..600 {
        let found = execution(router, id).await;
        let state = found["Status"]["State"].as_str().unwrap_or_default();
        if state != QUERY_STATE_QUEUED && state != QUERY_STATE_RUNNING {
            return found;
        }
        ::tokio::time::sleep(::std::time::Duration::from_millis(100)).await;
    }
    panic!("the execution {id} never reached a terminal state");
}

/// The ids `work_group`'s query history holds.
pub async fn history(router: &MultiServiceRouter, work_group: &str) -> Vec<String> {
    let (status, listed) = call(
        router,
        "ListQueryExecutions",
        &json!({ "WorkGroup": work_group, "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListQueryExecutions({work_group}) must answer 200, got {status} with {listed:?}"
    );
    listed["QueryExecutionIds"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
}

/// Create the workgroup `name`, with no configuration of its own.
pub async fn create_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(router, "CreateWorkGroup", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "the workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// A `ClientRequestToken` of the modelled length (32 to 128 characters), built from
/// `seed` so each caller carries its own.
pub fn request_token(seed: &str) -> String {
    format!("{seed:-<32}")
}

/// How long a wait for a transition a capacity reservation's clock owes may take.
///
/// Generous: what a caller asserts is the state the transition reaches, never how fast the
/// host reaches it, so the deadline is only there to fail a wait that will never end.
const RESERVATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How often such a wait looks again.
const RESERVATION_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Poll the capacity reservation `name` until it reports `status`, and answer with the
/// `CapacityReservation` that reported it.
///
/// A reservation's status follows from the instants its record holds, so a test waits for
/// the CONDITION — looking again until it holds — rather than sleeping out the span the
/// transition is due after.
pub async fn await_reservation_status(
    router: &MultiServiceRouter,
    name: &str,
    status: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + RESERVATION_DEADLINE;
    loop {
        let (code, body) = call(router, "GetCapacityReservation", &json!({ "Name": name })).await;
        let reservation = body["CapacityReservation"].clone();
        if code == 200 && reservation["Status"] == json!(status) {
            return reservation;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the reservation {name} must reach {status}, last read {body:?}"
        );
        tokio::time::sleep(RESERVATION_POLL).await;
    }
}

/// The wire error code (`__type`) of an error body, with the shape-id prefix the awsJson
/// protocols may carry stripped.
pub fn error_code(body: &Value) -> String {
    let raw = body["__type"].as_str().unwrap_or("<no __type>");
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}

/// Assert ONE refusal this service states itself: HTTP 400, the `InvalidRequestException`
/// envelope, the refusal CLASS `code` under BOTH of the names the wire gives it
/// (`AthenaErrorCode` and the `ErrorCode` alias), and `expected_message` word for word.
///
/// Shared here because that envelope is the same for every operation, so a suite asserting
/// it needs no copy of its own; `case` names the row under test in each failure.
pub fn assert_refusal(case: &str, status: u16, body: &Value, code: &str, expected_message: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        "InvalidRequestException",
        "case {case:?}: expected InvalidRequestException, got {body:?}"
    );
    for member in ["AthenaErrorCode", "ErrorCode"] {
        assert_eq!(
            body[member].as_str(),
            Some(code),
            "case {case:?}: expected {member} {code:?}, got {body:?}"
        );
    }
    assert_eq!(
        message(body),
        expected_message,
        "case {case:?}: expected the message {expected_message:?}, got {:?}",
        message(body)
    );
}

/// The refusal wording of an error body, under either casing of the `Message` member.
pub fn message(body: &Value) -> String {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or("<no Message>")
        .to_string()
}

/// One answer to a [`fetch`] of an absolute address this service handed out: the status, the
/// media type it was served under, and its exact bytes.
pub struct Fetched {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Follow an ABSOLUTE URL this service handed out, the way a client follows one: a bare
/// `GET` of the address itself, with no `Authorization` header and no body — so what is
/// driven is the request a browser or `curl` sends, not an SDK call.
///
/// Shared here because the dispatch path is the router's own: a suite that follows an
/// address this emulator published needs no transport of its own.
pub async fn fetch(router: &MultiServiceRouter, url: &str) -> Fetched {
    let request = http::Request::builder()
        .method("GET")
        .uri(url)
        .body(bytes::Bytes::new())
        .expect("a test fetch builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    Fetched {
        status,
        content_type,
        body: response.body().to_vec(),
    }
}
