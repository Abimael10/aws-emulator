// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a FUNCTION URL endpoint words a request it does not serve.
//!
//! RULE: a failure a function URL answers names the CLASS it belongs to in the
//! `x-amzn-errortype` header — the modelled exception `Invoke` declares against that status,
//! because a function URL request IS an invocation of the function — while the `502` a
//! handler's unusable response is answered with names no class at all and carries AWS's own
//! bare sentence, nothing of the function's failure.
//!
//! Every row drives a real HTTP request through the router, against a function whose
//! deployment package RUNS. The rows differ along what the rule generalises over: which
//! class of failure the request met (a caller the policy does not admit, a request past the
//! synchronous payload ceiling, a function whose concurrency reserves no slot for it) and
//! each way a handler's result is no response at all (an exception, a status outside the
//! HTTP range).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::function_url_data_plane::{
    control, given_configured_url, given_function, router_on, send, Answer, Endpoint,
};

/// The header a failure names its error class in, which a client branches on.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The bare sentence AWS answers every unusable response with.
const BAD_GATEWAY_BODY: &str = "Internal Server Error";

/// The handler every function below runs: it answers from the event's own `rawPath`, so one
/// function serves every row and each answer is the RUN's.
fn handler() -> String {
    r#"
def lambda_handler(event, context):
    path = event.get("rawPath", "/")
    if path == "/raise":
        raise RuntimeError("the handler failed")
    if path == "/bad-status":
        return {"statusCode": 99, "body": "out of range"}
    return {"statusCode": 200, "body": "served"}
"#
    .to_string()
}

/// `PutFunctionConcurrency` on `function`, which is what makes a function reserve the slots
/// an invocation of it is admitted from.
async fn reserve(router: &smithy_server_runtime::MultiServiceRouter, function: &str, slots: i64) {
    let answered = control(
        router,
        "PUT",
        &format!("/2017-10-31/functions/{function}/concurrency"),
        &json!({ "ReservedConcurrentExecutions": slots }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "PutFunctionConcurrency on {function:?} must succeed, got {}",
        answered.summary()
    );
}

/// One request to the endpoint, addressed host-style the way AWS publishes it.
async fn get(
    router: &smithy_server_runtime::MultiServiceRouter,
    endpoint: &Endpoint,
    path: &str,
    body: &[u8],
) -> Answer {
    send(router, "POST", &endpoint.host, path, body, &[]).await
}

/// One row of the class table: the case, the endpoint it addresses, the path and body it
/// sends, the status it is answered and the class that answer names, if any.
type ClassRow<'r> = (
    &'r str,
    &'r Endpoint,
    &'r str,
    &'r [u8],
    u16,
    Option<&'r str>,
);

/// RULE: the class of every failure this endpoint answers is the modelled exception of that
/// status, and a `502` names none.
#[tokio::test]
async fn a_function_url_names_the_class_of_every_failure_it_answers() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-classes", &handler()).await;
    // An `AWS_IAM` URL on a router that verifies nothing serves every caller, so the rows
    // below meet the failure each is about and not an authorization refusal.
    let served = given_configured_url(&router, "url-classes", None, "AWS_IAM", &Value::Null).await;
    // A `NONE` URL granted to nobody: its function's resource-based policy admits no
    // anonymous caller, which is the refusal this endpoint makes itself.
    given_function(&router, "url-unadmitted", &handler()).await;
    let unadmitted =
        given_configured_url(&router, "url-unadmitted", None, "NONE", &Value::Null).await;

    // "The maximum payload size is 6 MB for synchronous invocations", so a body past it is
    // one no invocation can carry.
    let oversized = vec![b'x'; 6 * 1024 * 1024 + 1];

    // (case, the endpoint, the path, the body, the status, the class it names)
    let rows: &[ClassRow<'_>] = &[
        (
            "a caller the function's policy does not admit",
            &unadmitted,
            "/probe",
            b"",
            403,
            Some("AccessDeniedException"),
        ),
        (
            "a request past the synchronous payload ceiling",
            &served,
            "/probe",
            &oversized,
            413,
            Some("RequestTooLargeException"),
        ),
        (
            "a handler that raised",
            &served,
            "/raise",
            b"",
            502,
            None,
        ),
        (
            "a status outside the HTTP range",
            &served,
            "/bad-status",
            b"",
            502,
            None,
        ),
    ];
    for (case, endpoint, path, body, status, class) in rows {
        let answer = get(&router, endpoint, path, body).await;
        assert_eq!(
            answer.status, *status,
            "case {case:?}: expected HTTP {status}, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.header(ERROR_TYPE_HEADER).as_deref(),
            *class,
            "case {case:?}: expected the class {class:?} in {ERROR_TYPE_HEADER}, got {:?}",
            answer.header(ERROR_TYPE_HEADER)
        );
        if *status == 502 {
            assert_eq!(
                answer.text(),
                BAD_GATEWAY_BODY,
                "case {case:?}: expected the bare sentence {BAD_GATEWAY_BODY:?}, got {}",
                answer.summary()
            );
        }
    }
}

/// RULE: a function whose reserved concurrency admits no invocation at all answers the
/// throttle class, which is what a client retries a throttle off.
#[tokio::test]
async fn a_function_url_request_no_slot_admits_names_the_throttle_class() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-throttled", &handler()).await;
    let endpoint =
        given_configured_url(&router, "url-throttled", None, "AWS_IAM", &Value::Null).await;
    // Reserving nothing leaves the function no slot an invocation can be admitted from.
    reserve(&router, "url-throttled", 0).await;

    let answer = get(&router, &endpoint, "/probe", b"").await;
    assert_eq!(
        answer.status,
        429,
        "case \"a function reserving no concurrency\": expected HTTP 429, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.header(ERROR_TYPE_HEADER).as_deref(),
        Some("TooManyRequestsException"),
        "case \"a function reserving no concurrency\": expected the throttle class in \
         {ERROR_TYPE_HEADER}, got {:?}",
        answer.header(ERROR_TYPE_HEADER)
    );
}
