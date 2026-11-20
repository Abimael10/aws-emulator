// SPDX-License-Identifier: CC0-1.0

//! What a `@pattern` violation of this service's input reports as the expression
//! the value had to satisfy, end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: the requirement clause quotes the member's modelled
//! `@pattern` exactly as the model spells it — the anchors included — because that
//! expression is the one this service matched the value against, and an expression
//! reported without its anchors describes values this service refuses. The refusal
//! is `ValidationException` (HTTP 400) over the constraint-clause template, and it
//! leaves the resource it was made against exactly as it was.
//!
//! The expressions below are the model's own
//! (`smithy_query(query="shape", shape="KmsKeyIdentifier,ConnectionName,RuleName")`),
//! written out here rather than imported, so a wrong constant in the
//! implementation cannot make a row pass.
//!
//! The rows span the class rather than one case: they differ along the operation
//! (five of them, over a create, two updates and two reads), the member (a key
//! identifier, a name, a name prefix), the shape whose pattern is reported, and
//! what makes the submitted value violate it (a character the expression admits
//! nowhere, a space, a leading marker).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The error code every refused input member of this service answers under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The modelled patterns the rows report, as the model spells them.
const KMS_KEY_IDENTIFIER_PATTERN: &str = r"^[a-zA-Z0-9_\-/:]*$";
const CONNECTION_NAME_PATTERN: &str = r"^[\.\-_A-Za-z0-9]+$";
const RULE_NAME_PATTERN: &str = r"^[\.\-_A-Za-z0-9]+$";

/// The connection the update rows are made against.
const CONNECTION: &str = "pattern-rows";
/// The description that connection is created with, which a refused update leaves
/// untouched.
const DESCRIPTION: &str = "the description a refused update must not change";

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

/// The whole message a `@pattern` violation must answer with: the envelope over
/// one clause naming the value that was rejected, the member path it sat at and
/// the expression it failed.
fn expected_message(value: &str, path: &str, pattern: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {pattern}"
    )
}

/// The `API_KEY` connection the update rows are made against, carrying a
/// description a refused update must leave alone.
async fn given_connection(router: &MultiServiceRouter) {
    let (status, body) = events(
        router,
        "CreateConnection",
        &json!({
            "Name": CONNECTION,
            "Description": DESCRIPTION,
            "AuthorizationType": "API_KEY",
            "AuthParameters": {
                "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "sekrit" },
            },
        }),
    )
    .await;
    assert_eq!(status, 200, "CreateConnection answered {status} {body}");
}

/// Every `@pattern` refusal reports the modelled expression as the model spells it.
#[tokio::test]
async fn a_pattern_refusal_quotes_the_modelled_expression() {
    let router = emulator();
    given_connection(&router).await;

    // (case, operation, request, the rejected value, the member path, the pattern)
    let rows: Vec<(&str, &str, Value, &str, &str, &str)> = vec![
        (
            "UpdateConnection's KmsKeyIdentifier",
            "UpdateConnection",
            json!({ "Name": CONNECTION, "KmsKeyIdentifier": "!!!not-a-kms-identifier!!!" }),
            "!!!not-a-kms-identifier!!!",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
        ),
        (
            "CreateConnection's KmsKeyIdentifier",
            "CreateConnection",
            json!({
                "Name": "with-a-bad-key",
                "AuthorizationType": "API_KEY",
                "AuthParameters": {
                    "ApiKeyAuthParameters": { "ApiKeyName": "k", "ApiKeyValue": "v" },
                },
                "KmsKeyIdentifier": "alias/not valid!",
            }),
            "alias/not valid!",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
        ),
        (
            "UpdateEventBus's KmsKeyIdentifier",
            "UpdateEventBus",
            json!({ "Name": "default", "KmsKeyIdentifier": "key#one" }),
            "key#one",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
        ),
        (
            "DescribeConnection's Name",
            "DescribeConnection",
            json!({ "Name": "not a connection name!" }),
            "not a connection name!",
            "name",
            CONNECTION_NAME_PATTERN,
        ),
        (
            "ListConnections' NamePrefix",
            "ListConnections",
            json!({ "NamePrefix": "not a prefix!" }),
            "not a prefix!",
            "namePrefix",
            CONNECTION_NAME_PATTERN,
        ),
        (
            "PutRule's Name",
            "PutRule",
            json!({ "Name": "not a rule name!" }),
            "not a rule name!",
            "name",
            RULE_NAME_PATTERN,
        ),
    ];

    for (case, operation, request, value, path, pattern) in &rows {
        let (status, refused) = events(&router, operation, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            VALIDATION_EXCEPTION,
            "case {case:?}: answered {refused}"
        );
        let expected = expected_message(value, path, pattern);
        let actual = error_message(&refused);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // The refused update left the connection exactly as it was: the description it
    // was created with, and the state it had reached.
    let (status, described) =
        events(&router, "DescribeConnection", &json!({ "Name": CONNECTION })).await;
    assert_eq!(
        status, 200,
        "DescribeConnection answered {status} {described}"
    );
    assert_eq!(
        described["Description"].as_str(),
        Some(DESCRIPTION),
        "a refused update leaves the description as it was: {described}"
    );
    assert!(
        described["KmsKeyIdentifier"].is_null(),
        "a refused update writes no key identifier: {described}"
    );
}
