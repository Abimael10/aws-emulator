// SPDX-License-Identifier: CC0-1.0

//! The HTTP STATUS this service answers a not-found refusal with.
//!
//! The rule every case here holds the service to: **a refusal carrying one of the error
//! shapes this model annotates `@httpError 404` — a resource, a request token or a registry
//! type that cannot be found — is answered HTTP 400, which is the status the recorded wire
//! responses of this awsJson1_0 service carry for all three, with the error code and the
//! `Message` still the modelled shape's; every other refusal keeps the status the model
//! gives it.**
//!
//! The rows differ along what the rule generalises over: which of the three shapes the
//! refusal carries, which operation answered it, the resource type, and the form of the
//! identifier, token or type name the request carried. The controls are the refusals the
//! model annotates with a status that is NOT 404 — which must still carry that status, so
//! what is asserted is a rule about the not-found shapes rather than "everything is 400" —
//! and the same calls over a resource, a token and a type that DO resolve.
//!
//! Shape source: the three `@httpError 404` shapes of this model
//! (`smithy_query(query="shape",shape="ResourceNotFoundException,RequestTokenNotFoundException,TypeNotFoundException")`);
//! every other error shape it declares is annotated 400, 401, 405, 409, 429 or 502.

use serde_json::{json, Value};

use super::emulator::{code, message, progress, text, Emulator};

/// The status this service answers a not-found refusal with.
const NOT_FOUND_STATUS: u16 = 400;

/// A type whose resources live in the CloudWatch Logs service's own store.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// A type whose resources live in this service's own store.
const STREAM: &str = "AWS::Kinesis::Stream";

/// A type whose primary identifier the caller supplies under another property name.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// A type whose own service words a not-found in its own terms.
const QUEUE: &str = "AWS::SQS::Queue";

/// A type whose own service's not-found sentence NAMES the resource.
const DYNAMODB_TABLE: &str = "AWS::DynamoDB::Table";

/// A type addressed by an ARN.
const TOPIC: &str = "AWS::SNS::Topic";

/// A patch that changes nothing: every case here is about the refusal, not the patch.
fn inert_patch() -> Value {
    json!([{ "op": "add", "path": "/Tags", "value": [] }])
}

/// The fixed text a handler-wrapped not-found brackets the owning handler's verdict with.
const WRAPPER_OPENING: &str = " Handler returned status FAILED: ";

/// The fixed text that carries the handler error code and the failing invocation.
const WRAPPER_VERDICT: &str = " (HandlerErrorCode: NotFound, RequestToken: ";

/// Check that `body` is the not-found refusal `expected_code`, answered with the status this
/// service answers one with.
fn assert_not_found(status: u16, body: &Value, expected_code: &str, case: &str) {
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got {status} with body {body:?}"
    );
    assert!(
        code(body).contains(expected_code),
        "case {case:?}: expected {expected_code}, got {:?}",
        code(body)
    );
    assert!(
        !message(body).is_empty(),
        "case {case:?}: the refusal must state a message, got {body:?}"
    );
    assert!(
        progress(body).is_null(),
        "case {case:?}: a refused request mints no resource operation, got {body:?}"
    );
}

#[tokio::test]
async fn an_absent_resource_is_refused_with_the_status_the_service_answers_a_not_found_with() {
    // The service that OWNS one of the types below is mounted beside this one, so that
    // type's rows are resolved through its own provider.
    let emulator = Emulator::with_cloudwatch_logs();
    // The rows differ along the type, where its resources would live (another service's
    // store, this service's own), and the form of the identifier: a plain name, a
    // path-shaped name, a name carrying dots, the primary-identifier JSON object the model
    // admits ("Primary identifiers can be specified as a string or JSON",
    // `GetResource$Identifier`), and an ARN.
    let rows = [
        (
            "a type whose resources live in the owning service's store",
            LOG_GROUP,
            "absent-group".to_string(),
        ),
        (
            "a path shaped identifier of that type",
            LOG_GROUP,
            "/aws/absent/group".to_string(),
        ),
        (
            "the primary identifier as JSON",
            LOG_GROUP,
            json!({ "LogGroupName": "absent-json-group" }).to_string(),
        ),
        (
            "a type whose resources live in this service's own store",
            STREAM,
            "absent-stream".to_string(),
        ),
        (
            "an identifier carrying dots",
            PARAMETER,
            "/absent.parameter.v2".to_string(),
        ),
        (
            "a type whose own service words a not-found in its own terms",
            QUEUE,
            "absent-queue".to_string(),
        ),
        (
            "a type whose service's sentence names the resource",
            DYNAMODB_TABLE,
            "absent-table".to_string(),
        ),
        (
            "an ARN identified type",
            TOPIC,
            "arn:aws:sns:us-east-1:123456789012:absent-topic".to_string(),
        ),
    ];

    for (row, type_name, identifier) in rows {
        // Both SYNCHRONOUS resource-level operations answer the refusal inline.
        let answers = [
            ("GetResource", emulator.get(type_name, &identifier).await),
            (
                "UpdateResource",
                emulator
                    .update(type_name, &identifier, &inert_patch(), None)
                    .await,
            ),
        ];
        for (operation, (status, body)) in answers {
            let case = format!("{row} ({operation})");
            assert_not_found(status, &body, "ResourceNotFoundException", &case);
            // The status changes nothing about the verdict the message carries.
            let stated = message(&body);
            let opening = format!("{type_name}{WRAPPER_OPENING}");
            assert!(
                stated.starts_with(&opening)
                    && stated.contains(WRAPPER_VERDICT)
                    && stated.ends_with(')'),
                "case {case:?}: expected a message opening {opening:?} and carrying \
                 {WRAPPER_VERDICT:?}, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_unknown_request_token_is_refused_with_the_status_the_service_answers_a_not_found_with() {
    let emulator = Emulator::with_cloudwatch_logs();
    // The rows differ along the SHAPE of a token nothing minted: the shortest and the
    // longest the model admits, a uuid, one made only of the punctuation the pattern
    // admits, and one of the form this service mints
    // (`RequestToken`, length 1..128, pattern ^[-A-Za-z0-9+/=]+$).
    let tokens = [
        ("the shortest token the model admits", "t".to_string()),
        ("the longest token the model admits", "t".repeat(128)),
        (
            "a uuid nothing minted",
            "3f8c6c4e-5d7a-4a1f-9a21-6f2b0c7d8e90".to_string(),
        ),
        (
            "only the punctuation the pattern admits",
            "-+/=".to_string(),
        ),
        (
            "the form this service mints",
            "cc-never-issued-00000001".to_string(),
        ),
    ];

    for (row, token) in tokens {
        // Both operations that resolve a token: the poll and the cancel.
        let answers = [
            ("GetResourceRequestStatus", emulator.status(&token).await),
            ("CancelResourceRequest", emulator.cancel(&token).await),
        ];
        for (operation, (status, body)) in answers {
            let case = format!("{row} ({operation})");
            assert_not_found(status, &body, "RequestTokenNotFoundException", &case);
            assert_eq!(
                message(&body),
                format!("Request with token {token} was not found"),
                "case {case:?}: unexpected wording"
            );
        }
    }
}

#[tokio::test]
async fn an_unregistered_type_is_refused_with_the_status_the_service_answers_a_not_found_with() {
    let emulator = Emulator::with_cloudwatch_logs();
    // The rows differ along the namespace the type name claims: an AWS one no service
    // publishes, a second such one, and two organizations of their own.
    let unregistered = [
        "AWS::NoSuchService::FakeResource",
        "AWS::SimParityProbe::NoSuchType",
        "Organisation::Service::Resource",
        "Mycorp::Custom::Widget",
    ];

    for type_name in unregistered {
        // Every operation that names a type asks the registry before anything else.
        let answers = [
            (
                "CreateResource",
                emulator
                    .create(type_name, &json!({ "Name": "whatever" }), None)
                    .await,
            ),
            ("GetResource", emulator.get(type_name, "whatever").await),
            (
                "ListResources",
                emulator
                    .cc(
                        "ListResources",
                        Emulator::list_request(type_name, None, None, None),
                    )
                    .await,
            ),
            (
                "UpdateResource",
                emulator
                    .update(type_name, "whatever", &inert_patch(), None)
                    .await,
            ),
            (
                "DeleteResource",
                emulator.delete(type_name, "whatever").await,
            ),
        ];
        for (operation, (status, body)) in answers {
            let case = format!("{type_name} ({operation})");
            assert_not_found(status, &body, "TypeNotFoundException", &case);
            assert_eq!(
                message(&body),
                format!("The type '{type_name}' cannot be found."),
                "case {case:?}: unexpected wording"
            );
        }
    }
}

#[tokio::test]
async fn a_refusal_the_model_gives_another_status_keeps_that_status() {
    let emulator = Emulator::new();

    // A type no resource provider stands behind: `UnsupportedActionException`, which the
    // model annotates 405.
    let unprovisionable = ::service_common::cfn_provisioning::NON_PROVISIONABLE_RESOURCE_TYPES
        .first()
        .expect("the registry publishes at least one type no provider stands behind");
    let (status, body) = emulator.get(unprovisionable, "does-not-matter").await;
    assert_eq!(
        status, 405,
        "an action no provider stands behind: expected HTTP 405, got {status} with body \
         {body:?}"
    );
    assert!(
        code(&body).contains("UnsupportedActionException"),
        "an action no provider stands behind: expected UnsupportedActionException, got {:?}",
        code(&body)
    );

    // A `ClientToken` that already named another request: `ClientTokenConflictException`,
    // which the model annotates 409.
    let client_token = "status-rule-token";
    let first = Emulator::create_request(
        STREAM,
        &json!({ "StreamName": "status-rule-stream", "ShardCount": 1 }),
        Some(client_token),
    );
    let _ = emulator.cc_ok("CreateResource", first).await;
    let second = Emulator::create_request(
        STREAM,
        &json!({ "StreamName": "another-stream", "ShardCount": 1 }),
        Some(client_token),
    );
    let (status, body) = emulator.cc("CreateResource", second).await;
    assert_eq!(
        status, 409,
        "a reused client token: expected HTTP 409, got {status} with body {body:?}"
    );
    assert!(
        code(&body).contains("ClientTokenConflictException"),
        "a reused client token: expected ClientTokenConflictException, got {:?}",
        code(&body)
    );

    // And the front door's own refusal, for a `TypeName` shorter than the model admits
    // (`TypeName`, length 10..196): `ValidationException`, which is 400 of its own right.
    let (status, body) = emulator.get("A::B::C", "whatever").await;
    assert_eq!(
        status, 400,
        "a type name the model refuses: expected HTTP 400, got {status} with body {body:?}"
    );
    assert!(
        code(&body).contains("ValidationException"),
        "a type name the model refuses: expected ValidationException, got {:?}",
        code(&body)
    );
}

#[tokio::test]
async fn a_resource_a_token_and_a_type_that_resolve_are_served() {
    let emulator = Emulator::new();
    // The same three calls as the suites above, over things that DO resolve: the rule is
    // about a refusal, so nothing it changes may turn a served call into one.
    let accepted = emulator
        .create_ok(
            STREAM,
            &json!({ "StreamName": "served-stream", "ShardCount": 1 }),
            None,
        )
        .await;
    let request_token = text(&accepted, "RequestToken");
    let (status, body) = emulator.status(&request_token).await;
    assert_eq!(
        status, 200,
        "a token this service minted must resolve, got {status} with body {body:?}"
    );

    let identifier = emulator.provision(STREAM, &json!({ "StreamName": "served-too", "ShardCount": 1 })).await;
    let (status, body) = emulator.get(STREAM, &identifier).await;
    assert_eq!(
        status, 200,
        "a provisioned resource must read back, got {status} with body {body:?}"
    );
    let (status, body) = emulator
        .cc("ListResources", Emulator::list_request(STREAM, None, None, None))
        .await;
    assert_eq!(
        status, 200,
        "a registered type must list, got {status} with body {body:?}"
    );
}
