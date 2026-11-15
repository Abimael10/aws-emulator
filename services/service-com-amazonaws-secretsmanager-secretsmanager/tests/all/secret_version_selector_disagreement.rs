// SPDX-License-Identifier: CC0-1.0

//! A value read that pins BOTH version selectors is refused for the STATE of the secret when
//! they do not agree, not as a value that cannot be found.
//!
//! The rule under test: "If you include both this parameter and `VersionId`, the two
//! parameters must refer to the same secret version."
//! (`GetSecretValueRequest$VersionStage`, this service's Smithy model.) So when the
//! `VersionId` names a version of the secret and the `VersionStage` beside it is not one of
//! the staging labels that version carries, the request contradicts itself: the refusal is
//! 400 `InvalidRequestException` with the fixed sentence "You provided a VersionStage that is
//! not associated to the provided VersionId.", whether the label is attached to some OTHER
//! version or to no version at all. Only a `VersionId` that names no version of the secret is
//! a miss, answered 400 `ResourceNotFoundException` naming the id it resolved by.
//!
//! Every test is a TABLE over what the rule generalises over: whether the label is reserved
//! (`AWSCURRENT`, `AWSPREVIOUS`, `AWSPENDING`) or one the caller named, whether it is attached
//! to another version or to none, which version the id names, how many versions the secret
//! holds, and the form of the `SecretId`.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::{
    value_version_not_found_message, VERSION_ID_AND_STAGE_DISAGREE_MESSAGE,
};
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 header naming `secretsmanager` in `us-west-2`; the router routes on the credential
/// scope and the emulator verifies signatures only when an authenticator is registered, which
/// this harness does not do.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-west-2/secretsmanager/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

/// The published minimum of `SecretVersionIdType` / `ClientRequestTokenType` (`@length`
/// 32..64, this service's Smithy model), which every version id in this suite meets.
const VERSION_ID_MIN_LEN: usize = 32;

/// A fresh emulator holding only this service, over its own engine.
fn emulator() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one `awsJson1_1` request and return its status and JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", AUTHORIZATION)
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body is serializable"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Send one request that must succeed, and return its body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = call(router, operation, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {operation}, got {status} with body {parsed}"
    );
    parsed
}

/// Send one request that must be refused, and return its status, wire error code and message.
async fn err(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) -> (u16, String, String) {
    let (status, parsed) = call(router, operation, &body).await;
    let raw = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no __type in error body {parsed}"));
    let code = raw.split('#').next_back().unwrap_or(raw).to_string();
    let message = parsed
        .get("message")
        .or_else(|| parsed.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no message in error body {parsed}"))
        .to_string();
    (status, code, message)
}

/// A version id (and so a `ClientRequestToken`) of at least the modelled minimum length,
/// unique per `seed`.
fn version_id(seed: &str) -> String {
    let mut id = format!("v-{seed}-");
    while id.len() < VERSION_ID_MIN_LEN {
        id.push('z');
    }
    id
}

/// A partial ARN: the secret's ARN with the six random characters its name is suffixed with
/// cut off, which `SecretId` also accepts.
fn partial_arn(arn: &str) -> String {
    arn.rsplit_once('-')
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| arn.to_string())
}

/// A secret holding two versions: the one `CreateSecret` minted, which `PutSecretValue` then
/// pushes to `AWSPREVIOUS`, and the one that now holds `AWSCURRENT`. Answers
/// `(ARN, first version id, second version id)`.
async fn secret_with_two_versions(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
) -> (String, String, String) {
    let first = version_id(&format!("first-{name}"));
    let second = version_id(&format!("second-{name}"));
    let created = ok(
        router,
        "CreateSecret",
        json!({"Name": name, "SecretString": "value-0", "ClientRequestToken": first.clone()}),
        case,
    )
    .await;
    ok(
        router,
        "PutSecretValue",
        json!({"SecretId": name, "SecretString": "value-1", "ClientRequestToken": second.clone()}),
        case,
    )
    .await;
    let arn = created
        .get("ARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no ARN in {created}"))
        .to_string();
    (arn, first, second)
}

#[tokio::test]
async fn a_version_stage_the_named_version_does_not_carry_is_refused_for_the_state() {
    let router = emulator();
    let name = "selectors/disagree";
    let (arn, first, second) = secret_with_two_versions(&router, name, "setup").await;
    // A staging label of the caller's own on the FIRST version, so one row can name a
    // caller-named label attached to another version; `blue` is a label this service never
    // moves itself.
    ok(
        &router,
        "UpdateSecretVersionStage",
        json!({"SecretId": name, "VersionStage": "blue", "MoveToVersionId": first.clone()}),
        "setup",
    )
    .await;

    // (case, the VersionId pinned, the VersionStage pinned, how the SecretId is spelled)
    let rows: Vec<(String, String, &str, String)> = vec![
        (
            "the current version under AWSPREVIOUS, which the first version carries".to_string(),
            second.clone(),
            "AWSPREVIOUS",
            name.to_string(),
        ),
        (
            "the previous version under AWSCURRENT, which the second version carries".to_string(),
            first.clone(),
            "AWSCURRENT",
            name.to_string(),
        ),
        (
            "AWSPENDING, which no version of the secret carries".to_string(),
            second.clone(),
            "AWSPENDING",
            name.to_string(),
        ),
        (
            "a caller-named label attached to the other version".to_string(),
            second.clone(),
            "blue",
            arn.clone(),
        ),
        (
            "a caller-named label attached to no version at all".to_string(),
            first.clone(),
            "green",
            partial_arn(&arn),
        ),
    ];

    for (case, pinned_version, pinned_stage, secret_id) in &rows {
        let body = json!({
            "SecretId": secret_id,
            "VersionId": pinned_version,
            "VersionStage": pinned_stage,
        });
        let (status, code, message) = err(&router, "GetSecretValue", body.clone(), case).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {body}, got {status} with message {message:?}"
        );
        assert_eq!(
            code, "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException for {body}, got {code:?} with \
             message {message:?}"
        );
        assert_eq!(
            message, VERSION_ID_AND_STAGE_DISAGREE_MESSAGE,
            "case {case:?}: expected {VERSION_ID_AND_STAGE_DISAGREE_MESSAGE:?}, got {message:?}"
        );
    }

    // None of those refusals changed what the secret holds: both versions are still there,
    // under the labels they had.
    let described = ok(
        &router,
        "DescribeSecret",
        json!({"SecretId": name}),
        "after the refusals",
    )
    .await;
    let stages = described
        .get("VersionIdsToStages")
        .and_then(Value::as_object)
        .unwrap_or_else(|| {
            panic!("case \"after the refusals\": no VersionIdsToStages in {described}")
        });
    let mut first_stages: Vec<&str> = stages
        .get(&first)
        .and_then(Value::as_array)
        .map(|labels| labels.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    first_stages.sort_unstable();
    assert_eq!(
        first_stages,
        vec!["AWSPREVIOUS", "blue"],
        "case \"after the refusals\": expected the first version's labels untouched, got \
         {described}"
    );
    assert_eq!(
        stages.get(&second),
        Some(&json!(["AWSCURRENT"])),
        "case \"after the refusals\": expected the current version's labels untouched, got \
         {described}"
    );
}

#[tokio::test]
async fn selectors_that_agree_are_served_and_a_version_id_miss_is_not_found() {
    let router = emulator();
    let name = "selectors/agree";
    let (arn, first, second) = secret_with_two_versions(&router, name, "setup").await;
    ok(
        &router,
        "UpdateSecretVersionStage",
        json!({"SecretId": name, "VersionStage": "blue", "MoveToVersionId": first.clone()}),
        "setup",
    )
    .await;

    // (case, the VersionId pinned, the VersionStage pinned, the value expected back)
    let served: Vec<(&str, String, &str, &str)> = vec![
        (
            "the current version under AWSCURRENT",
            second.clone(),
            "AWSCURRENT",
            "value-1",
        ),
        (
            "the first version under AWSPREVIOUS",
            first.clone(),
            "AWSPREVIOUS",
            "value-0",
        ),
        (
            "the first version under the caller's own label",
            first.clone(),
            "blue",
            "value-0",
        ),
    ];
    for (case, pinned_version, pinned_stage, expected_value) in &served {
        let answered = ok(
            &router,
            "GetSecretValue",
            json!({
                "SecretId": name,
                "VersionId": pinned_version,
                "VersionStage": pinned_stage,
            }),
            case,
        )
        .await;
        assert_eq!(
            answered.get("VersionId").and_then(Value::as_str),
            Some(pinned_version.as_str()),
            "case {case:?}: expected the pinned version back, got {answered}"
        );
        assert_eq!(
            answered.get("SecretString").and_then(Value::as_str),
            Some(*expected_value),
            "case {case:?}: expected the pinned version's value {expected_value:?}, got {answered}"
        );
    }

    // A `VersionId` that names NO version of the secret is a miss however the request spells
    // the rest of itself: the read reports the value it could not find and the member it
    // resolved by.
    let absent = version_id("absent");
    let missing: Vec<(&str, Value)> = vec![
        (
            "an absent VersionId alone",
            json!({"SecretId": name, "VersionId": absent.clone()}),
        ),
        (
            "an absent VersionId beside a label a version does carry",
            json!({"SecretId": name, "VersionId": absent.clone(), "VersionStage": "AWSCURRENT"}),
        ),
        (
            "an absent VersionId beside a label no version carries, by ARN",
            json!({"SecretId": arn, "VersionId": absent.clone(), "VersionStage": "green"}),
        ),
    ];
    for (case, body) in &missing {
        let (status, code, message) = err(&router, "GetSecretValue", body.clone(), case).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {body}, got {status} with message {message:?}"
        );
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException for {body}, got {code:?} with \
             message {message:?}"
        );
        assert_eq!(
            message,
            value_version_not_found_message(&absent),
            "case {case:?}: expected the VersionId miss sentence, got {message:?}"
        );
    }
}
