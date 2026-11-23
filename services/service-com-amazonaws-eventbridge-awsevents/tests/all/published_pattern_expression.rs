// SPDX-License-Identifier: CC0-1.0

//! Which EXPRESSION a `@pattern` violation of this service's input quotes, end to end over
//! the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the
//! real `MultiServiceRouter`.
//!
//! The RULE asserted here: the requirement clause quotes the expression this service
//! PUBLISHES for the shape the member targets — the model's own text, the whole-value
//! `^`/`$` pair included, for a shape this service publishes as declared, and the
//! expression inside that pair for every other shape. What is published belongs to the
//! SHAPE, so it holds for every member targeting it: the same shape is quoted the same way
//! in a create, in an update, in a read, and at any depth, and two members of ONE operation
//! targeting differently published shapes are quoted differently. The refusal itself is
//! `ValidationException` (HTTP 400) over the constraint-clause template, and it leaves the
//! resource it was made against exactly as it was.
//!
//! The expressions below are the model's own
//! (`smithy_query(query="shape",
//! shape="KmsKeyIdentifier,EventSourceName,RuleName,ConnectionName,TargetId")`), written out
//! here rather than imported, so a wrong constant in the implementation cannot make a row
//! pass; so is the drop of the pair, which the rows spell out themselves.
//!
//! The rows span the class rather than one case: they differ along the shape (five of them,
//! both published forms), the operation (nine, over creates, updates and reads), the member
//! (a key identifier, a name, an event-source name, a list element), the depth the member
//! sits at, and what makes the submitted value violate its expression (a character the
//! expression admits nowhere, a space, a missing segment).

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
const EVENT_SOURCE_NAME_PATTERN: &str = r"^aws\.partner(/[\.\-_A-Za-z0-9]+){2,}$";
const CONNECTION_NAME_PATTERN: &str = r"^[\.\-_A-Za-z0-9]+$";
const RULE_NAME_PATTERN: &str = r"^[\.\-_A-Za-z0-9]+$";
const TARGET_ID_PATTERN: &str = r"^[\.\-_A-Za-z0-9]+$";

/// An ARN of this service's own event-bus form, so an archive row violates the one member
/// it is about and nothing else.
const EVENT_BUS_ARN: &str = "arn:aws:events:us-west-2:123456789012:event-bus/default";

/// The connection the update rows are made against.
const CONNECTION: &str = "published-pattern-rows";
/// The description that connection is created with, which a refused update leaves untouched.
const DESCRIPTION: &str = "the description a refused update must not change";
/// The description a LATER, acceptable update sets, which proves the refusal left the
/// connection updatable.
const NEW_DESCRIPTION: &str = "the description an accepted update does set";

/// Which of the two forms this service publishes an expression in.
#[derive(Debug, Clone, Copy)]
enum Quoting {
    /// The model's own text, the whole-value anchoring pair included.
    AsDeclared,
    /// The expression inside the whole-value pair the model writes around it.
    WithoutTheWholeValuePair,
}

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

/// The wire error code of a failure body (`__type` on awsJson), without its namespace.
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

/// The expression a clause must quote for `model_pattern`, in the form this service
/// publishes that expression in.
///
/// The drop of the whole-value pair is spelled out here rather than imported, so the
/// implementation's own reading of the wrapping cannot make a row pass, and it panics on a
/// constant that is not a whole-value expression, so a model text that changed shape fails
/// this suite rather than quietly weakening it.
fn published_expression(model_pattern: &str, quoting: Quoting) -> &str {
    let body = model_pattern
        .strip_prefix('^')
        .and_then(|inside| inside.strip_suffix('$'))
        .unwrap_or_else(|| {
            panic!("the modelled pattern {model_pattern:?} must wrap the whole value")
        });
    match quoting {
        Quoting::AsDeclared => model_pattern,
        Quoting::WithoutTheWholeValuePair => body,
    }
}

/// The whole message a `@pattern` violation must answer with: the envelope over one clause
/// naming the value that was rejected, the member path it sat at and the expression it
/// failed, as this service publishes that expression.
fn expected_message(value: &str, path: &str, model_pattern: &str, quoting: Quoting) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {}",
        published_expression(model_pattern, quoting)
    )
}

/// The `API_KEY` connection the update rows are made against, carrying a description a
/// refused update must leave alone.
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

/// Every `@pattern` refusal quotes the expression this service publishes for the member's
/// shape, in the form that shape is published in.
#[tokio::test]
async fn a_pattern_refusal_quotes_the_expression_the_shape_is_published_with() {
    let router = emulator();
    given_connection(&router).await;

    // (case, operation, request, the rejected value, the member path, the modelled
    // expression, the form this service publishes it in)
    let rows: Vec<(&str, &str, Value, &str, &str, &str, Quoting)> = vec![
        // The key-identifier shape, published as the model declares it, over every
        // operation that takes one: a connection's, an event bus's and an archive's.
        (
            "UpdateConnection's KmsKeyIdentifier",
            "UpdateConnection",
            json!({ "Name": CONNECTION, "KmsKeyIdentifier": "!!!not-a-kms-identifier!!!" }),
            "!!!not-a-kms-identifier!!!",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
            Quoting::AsDeclared,
        ),
        (
            "CreateConnection's KmsKeyIdentifier carrying a space",
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
            Quoting::AsDeclared,
        ),
        (
            "CreateEventBus's KmsKeyIdentifier",
            "CreateEventBus",
            json!({ "Name": "a-new-bus", "KmsKeyIdentifier": "key#one" }),
            "key#one",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
            Quoting::AsDeclared,
        ),
        (
            "UpdateEventBus's KmsKeyIdentifier",
            "UpdateEventBus",
            json!({ "Name": "default", "KmsKeyIdentifier": "key one" }),
            "key one",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
            Quoting::AsDeclared,
        ),
        (
            "CreateArchive's KmsKeyIdentifier",
            "CreateArchive",
            json!({
                "ArchiveName": "an-archive",
                "EventSourceArn": EVENT_BUS_ARN,
                "KmsKeyIdentifier": "key?two",
            }),
            "key?two",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
            Quoting::AsDeclared,
        ),
        (
            "UpdateArchive's KmsKeyIdentifier",
            "UpdateArchive",
            json!({ "ArchiveName": "an-archive", "KmsKeyIdentifier": "alias.three" }),
            "alias.three",
            "kmsKeyIdentifier",
            KMS_KEY_IDENTIFIER_PATTERN,
            Quoting::AsDeclared,
        ),
        // Every other shape of this model is published as the expression inside the pair —
        // including the SECOND pattern-constrained member of an operation that also takes a
        // key identifier, which is what makes the form a property of the shape rather than
        // of the operation.
        (
            "CreateEventBus's EventSourceName, missing a namespace segment",
            "CreateEventBus",
            json!({ "Name": "another-bus", "EventSourceName": "aws.partner/onlyone" }),
            "aws.partner/onlyone",
            "eventSourceName",
            EVENT_SOURCE_NAME_PATTERN,
            Quoting::WithoutTheWholeValuePair,
        ),
        (
            "DescribeConnection's Name",
            "DescribeConnection",
            json!({ "Name": "not a connection name!" }),
            "not a connection name!",
            "name",
            CONNECTION_NAME_PATTERN,
            Quoting::WithoutTheWholeValuePair,
        ),
        (
            "PutRule's Name",
            "PutRule",
            json!({ "Name": "not a rule name!" }),
            "not a rule name!",
            "name",
            RULE_NAME_PATTERN,
            Quoting::WithoutTheWholeValuePair,
        ),
        (
            "RemoveTargets' Ids element",
            "RemoveTargets",
            json!({ "Rule": "a-rule", "Ids": ["bad id!"] }),
            "bad id!",
            "ids.1.member",
            TARGET_ID_PATTERN,
            Quoting::WithoutTheWholeValuePair,
        ),
    ];

    for (case, operation, request, value, path, pattern, quoting) in &rows {
        let (status, refused) = events(&router, operation, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused}"
        );
        let code = error_code(&refused);
        assert_eq!(
            code, VALIDATION_EXCEPTION,
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {code:?} in {refused}"
        );
        let expected = expected_message(value, path, pattern, *quoting);
        let actual = error_message(&refused);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // The refused update left the connection exactly as it was, and left it updatable: the
    // description it was created with, no key identifier, and a later description-only
    // update that succeeds and does change it.
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

    let (status, updated) = events(
        &router,
        "UpdateConnection",
        &json!({ "Name": CONNECTION, "Description": NEW_DESCRIPTION }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the accepted update answered {status} {updated}"
    );
    let (status, described) =
        events(&router, "DescribeConnection", &json!({ "Name": CONNECTION })).await;
    assert_eq!(
        status, 200,
        "DescribeConnection answered {status} {described}"
    );
    assert_eq!(
        described["Description"].as_str(),
        Some(NEW_DESCRIPTION),
        "the accepted update changed the description: {described}"
    );
}
