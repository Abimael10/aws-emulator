// SPDX-License-Identifier: CC0-1.0

//! What a paginated listing of this service answers a `NextToken` it cannot resume
//! from, end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: a `NextToken` this service cannot resume from is
//! refused with `ValidationException` (HTTP 400) whose message names the parameter
//! that was rejected and states the reason a refused parameter of this service
//! carries — the request is malformed — and nothing of the token, the listing or
//! the position it named. One sentence for every paginated listing and every way a
//! token is unusable, because it is one answer: this listing cannot resume from it.
//!
//! `NextToken` is modelled `@length 1..2048` and carries no pattern, so a token
//! inside those bounds reaches the handler and the handler is what decides it
//! (`smithy_query(query="operation", operation="ListConnections")`); a `Limit`
//! outside `LimitMax100`'s modelled `@range 1..100` is refused at the boundary
//! instead, which is the control row below: its own constraint clause is unchanged
//! by the sentence above.
//!
//! The rows span the class rather than one case: they differ along the listing
//! asked (five of them, one of which takes a filter beside the token), and along
//! WHY the token is unusable — text that is no token of this service at all, text
//! that decodes but names no position, an empty-ish token, and a token this
//! service really issued whose position is gone by the time it comes back.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The whole message a listing answers a token it cannot resume from. Written out
/// here rather than imported, so a wrong constant in the implementation cannot
/// make a row pass.
const INVALID_NEXT_TOKEN: &str =
    "Parameter NextToken is not valid. Reason: The request is malformed.";

/// The error code every refused parameter of this service answers under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One `awsJson1_1` call to this service.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let credential = format!(
        "AKIDTESTTESTTEST/20260101/{REGION}/events/aws4_request, SignedHeaders=host, \
         Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!("AWS4-HMAC-SHA256 Credential={credential}"),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&payload)
            )
        })
    };
    (status, parsed)
}

/// The wire error code of a failure body (`__type` on awsJson), without its
/// namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
        .unwrap_or_default()
}

/// The `message` a failure body carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Three `API_KEY` connections, so the listings below have something to page
/// through.
async fn given_connections(router: &MultiServiceRouter, count: usize) {
    for index in 0..count {
        let (status, body) = events(
            router,
            "CreateConnection",
            &json!({
                "Name": format!("paged-{index}"),
                "AuthorizationType": "API_KEY",
                "AuthParameters": {
                    "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "k" },
                },
            }),
        )
        .await;
        assert_eq!(status, 200, "CreateConnection answered {status} {body}");
    }
}

/// Every paginated listing refuses a token it cannot resume from in exactly those
/// words, whatever the token looked like.
#[tokio::test]
async fn an_unresumable_next_token_is_refused_with_the_reason_clause() {
    let router = emulator();
    given_connections(&router, 3).await;

    // The token forms differ along WHY this service cannot resume from them: text
    // that is not one of its tokens at all, text that decodes but carries no
    // position (`7b2262223a2278227d` is the hex of `{"b":"x"}`), and an empty-ish
    // token.
    let tokens = ["not-a-token", "7b2262223a2278227d", "zzzz", "0"];
    // (the listing, the request members it needs beside NextToken)
    let listings: [(&str, Value); 5] = [
        ("ListConnections", json!({})),
        ("ListConnections", json!({ "NamePrefix": "paged" })),
        ("ListRules", json!({})),
        ("ListEventBuses", json!({})),
        ("ListArchives", json!({})),
    ];

    for (operation, members) in &listings {
        for token in tokens {
            let case = format!("{operation} resumed from {token:?} with {members}");
            let mut request = members.clone();
            request["NextToken"] = json!(token);
            let (status, refused) = events(&router, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {refused}"
            );
            assert_eq!(
                error_code(&refused),
                VALIDATION_EXCEPTION,
                "case {case:?}: answered {refused}"
            );
            let actual = error_message(&refused);
            assert_eq!(
                actual, INVALID_NEXT_TOKEN,
                "case {case:?}: expected {INVALID_NEXT_TOKEN:?}, got {actual:?}"
            );
        }
    }
}

/// A token this service DID issue resumes the listing, so the sentence above is
/// this service's answer about a token it cannot resume from and not about every
/// token a request carries.
#[tokio::test]
async fn a_token_this_service_issued_resumes_the_listing() {
    let router = emulator();
    given_connections(&router, 3).await;

    let (status, page) = events(&router, "ListConnections", &json!({ "Limit": 1 })).await;
    assert_eq!(status, 200, "the first page answered {status} {page}");
    let token = page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a first page of three connections carries a NextToken: {page}"))
        .to_string();

    let (status, resumed) = events(
        &router,
        "ListConnections",
        &json!({ "Limit": 1, "NextToken": &token }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a token this service issued resumes the listing, got {status} {resumed}"
    );
    assert_eq!(
        resumed["Connections"].as_array().map(Vec::len),
        Some(1),
        "the resumed page carries the next connection: {resumed}"
    );
}

/// The control: a `Limit` outside the modelled range keeps the constraint clause
/// the boundary states for it, so the token sentence above is this service's answer
/// about the TOKEN and not a rewording of every refusal the listing makes.
#[tokio::test]
async fn a_limit_outside_its_modelled_range_keeps_its_own_clause() {
    let router = emulator();
    // (case, the Limit, the bound the clause states)
    let rows = [
        ("above the maximum", 101, "less than or equal to 100"),
        ("below the minimum", 0, "greater than or equal to 1"),
    ];
    for (case, limit, bound) in rows {
        let (status, refused) = events(&router, "ListConnections", &json!({ "Limit": limit })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            VALIDATION_EXCEPTION,
            "case {case:?}: answered {refused}"
        );
        let expected = format!(
            "1 validation error detected: Value '{limit}' at 'limit' failed to satisfy \
             constraint: Member must have value {bound}"
        );
        let actual = error_message(&refused);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}
