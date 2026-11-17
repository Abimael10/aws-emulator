// SPDX-License-Identifier: CC0-1.0

//! How `TestState` refuses a `stateName` the definition it was handed declares no state
//! of.
//!
//! RULE: `TestState` "denotes the particular state within a state machine definition to be
//! tested", so a `stateName` naming no state of the submitted definition is a defect in the
//! request: the call is refused as the modelled `ValidationException` (HTTP 400) carrying
//! the one fixed sentence `State not found in definition`, with no part of the request
//! interpolated into it — the name that was asked for is not in the message, so a client
//! matching on the refusal text reads the same sentence whichever name it asked about. A
//! `stateName` the definition DOES declare names the state under test, and that state is
//! run.
//!
//! The rows differ along what the rule generalises over: a name no state is declared
//! under, one that differs from a declared name only in case (state names are matched
//! exactly), one at the far end of the member's own length bound
//! (`smithy_query(query="shape",shape="TestStateStateName")` — `@length 1..80`), one whose
//! characters are punctuation and spaces, and the name of a state the definition really
//! declares, which is the row that is served instead of refused.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The refusal this operation answers such a request with: the modelled client error and
/// the whole of its message, as AWS words it.
const VALIDATION_EXCEPTION: &str = "ValidationException";
/// See [`VALIDATION_EXCEPTION`].
const NOT_FOUND_MESSAGE: &str = "State not found in definition";

/// The longest a `stateName` may be: `TestStateStateName` is `@length 1..80`.
const LONGEST_STATE_NAME: usize = 80;

/// The states the definition under test declares, and what each of them does.
const DECLARED: &str = "Approve";
/// See [`DECLARED`].
const ALSO_DECLARED: &str = "Reject";

/// The emulator as it is ASSEMBLED, which is where a client reaches this operation.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// One `TestState` call, as its status and parsed body.
async fn test_state(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    let host = format!("states.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", "AWSStepFunctions.TestState")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/states/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let parsed = serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("TestState must answer a JSON document, got {error} for {text:?}")
    });
    (status, parsed)
}

/// The fully-formed state machine definition every row submits: two states, so a row's
/// `stateName` is judged against a definition that declares more than one.
fn definition() -> String {
    let mut states = serde_json::Map::new();
    states.insert(
        DECLARED.to_string(),
        json!({ "Type": "Pass", "Result": { "approved": true }, "Next": ALSO_DECLARED }),
    );
    states.insert(
        ALSO_DECLARED.to_string(),
        json!({ "Type": "Pass", "End": true }),
    );
    json!({ "StartAt": DECLARED, "States": Value::Object(states) }).to_string()
}

/// A `stateName` the definition declares no state of is refused with AWS's own fixed
/// sentence, whatever name was asked for; one it declares is tested instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_undeclared_state_name_is_refused_with_the_fixed_sentence() {
    let router = build_router();

    // (case, the `stateName` the request names, whether the definition declares it)
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "a name no state is declared under",
            "Wait30Seconds".to_string(),
            false,
        ),
        (
            "a declared name in another case, which names no state",
            DECLARED.to_lowercase(),
            false,
        ),
        (
            "a name at the far end of the member's length bound",
            "s".repeat(LONGEST_STATE_NAME),
            false,
        ),
        (
            "a name of punctuation and spaces",
            "- . _ ( ) :".to_string(),
            false,
        ),
        ("a state the definition declares", DECLARED.to_string(), true),
        (
            "the other state the definition declares",
            ALSO_DECLARED.to_string(),
            true,
        ),
    ];

    for (case, state_name, declared) in rows {
        let (status, answered) = test_state(
            &router,
            &json!({
                "definition": definition(),
                "stateName": state_name,
                "input": json!({ "order": "o-1" }).to_string(),
                "roleArn": "arn:aws:iam::000000000000:role/TestStateRole",
            }),
        )
        .await;
        if declared {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the declared state to be tested, got {status} with \
                 {answered}"
            );
            assert_eq!(
                answered.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the state to have run, got {answered}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected the refusal of an undeclared state, got {status} with \
             {answered}"
        );
        let discriminator = answered
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            discriminator.ends_with(VALIDATION_EXCEPTION),
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {answered}"
        );
        let message = answered
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            message, NOT_FOUND_MESSAGE,
            "case {case:?}: expected the fixed sentence, got {message:?}"
        );
        assert!(
            !message.contains(state_name.as_str()),
            "case {case:?}: expected no part of the request in the message, got {message:?}"
        );
    }
}
