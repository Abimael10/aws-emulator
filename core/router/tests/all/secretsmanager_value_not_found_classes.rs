// SPDX-License-Identifier: CC0-1.0

//! The two not-found classes of a Secrets Manager value read are told apart by their message.
//!
//! The rule under test: **a read of a secret value answers `ResourceNotFoundException` for two
//! different misses, and because the wire error code is the same for both, the MESSAGE is the
//! only thing that tells them apart** — a `SecretId` the store holds no secret under carries
//! the one fixed missing-secret sentence ("Secrets Manager can't find the specified secret."),
//! whatever version the request named, while a secret that IS held and holds no version under
//! the requested `VersionId` carries the value sentence, which names the member the read
//! resolved by and that member's value ("Secrets Manager can't find the specified secret value
//! for VersionId: <VersionId>"). Only the `VersionId` echoed from the request varies.
//!
//! The tables run over what the rule generalises over: the state the secret is in (how many
//! versions it holds and where its staging labels sit, including a candidate left labelled
//! `AWSPENDING` beside `AWSCURRENT` — what a rotation whose finishing step never ran leaves
//! behind), the form and length of the absent `VersionId` (`SecretVersionIdType` is modelled
//! with `@length` 32..64, so the rows run over both published bounds and a value between
//! them), the form of the `SecretId` the read addresses the secret by, and which version
//! member — if any — a read of a missing secret names.
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate still roots its integration tests at a hand-declared `tests/all.rs`, which a
//! fix may not edit; the router's `tests/all/` picks a new suite up with nothing to declare.
//! Every call goes through a router with the owning service mounted on one shared engine,
//! exactly as the production router mounts it, so each assertion exercises the real wire path.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::ROTATION_IN_PROGRESS_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 header naming `secretsmanager` in `us-west-2`: the routing key this service claims
/// on, and the source of the Region its ARNs carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-west-2/secretsmanager/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

/// The one sentence this service answers a secret it cannot find with.
const SECRET_NOT_FOUND: &str = "Secrets Manager can't find the specified secret.";

/// The ARN of a Lambda rotation function, as `RotationLambdaARN` carries it.
const LAMBDA_ARN: &str = "arn:aws:lambda:us-west-2:123456789012:function:RotateMySecret";

/// The published bounds of `SecretVersionIdType` (`@length` 32 and 64, the Secrets Manager
/// Smithy model), which `ClientRequestToken` shares.
const VERSION_ID_MIN_LEN: usize = 32;
const VERSION_ID_MAX_LEN: usize = 64;

/// A fresh emulator: one router with Secrets Manager mounted over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
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

/// A required string member of a response body.
fn text(parsed: &Value, member: &str, case: &str) -> String {
    parsed
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {parsed}"))
        .to_string()
}

/// A version id (and so a `ClientRequestToken`) of `length` characters, unique per `seed`.
///
/// The whole seed has to fit inside `length`, or two seeds would pad to the same id and a row
/// would silently read the version another row wrote.
fn version_id(seed: &str, length: usize) -> String {
    let mut id = format!("v-{seed}-");
    assert!(
        id.len() <= length,
        "seed {seed:?} needs {} characters, more than the {length} asked for",
        id.len()
    );
    while id.len() < length {
        id.push('z');
    }
    id
}

/// The expected sentence for a read that resolved by `VersionId` and found no version.
fn expected_for_version_id(version_id: &str) -> String {
    format!("Secrets Manager can't find the specified secret value for VersionId: {version_id}")
}

/// The staging map `DescribeSecret` reports for `secret_id`.
async fn staging_map(router: &MultiServiceRouter, secret_id: &str, case: &str) -> Value {
    let described = ok(router, "DescribeSecret", json!({"SecretId": secret_id}), case).await;
    described
        .get("VersionIdsToStages")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no VersionIdsToStages in {described}"))
}

/// How many versions a secret holds and where its staging labels sit, for one table row.
#[derive(Clone, Copy, Debug)]
enum Layout {
    /// One version, holding `AWSCURRENT`.
    OneVersion,
    /// A second value stored, so `AWSCURRENT` moved on and `AWSPREVIOUS` stayed behind.
    CurrentAndPrevious,
    /// A candidate version labelled `AWSPENDING` beside the `AWSCURRENT` one: what a rotation
    /// whose finishing step never ran leaves behind.
    StalePendingCandidate,
}

/// Build a secret in `layout`, and report its ARN, the version id a later read can resolve,
/// and — where the layout has one — the candidate carrying `AWSPENDING`.
async fn secret_in(
    router: &MultiServiceRouter,
    name: &str,
    layout: Layout,
    case: &str,
) -> (String, String, Option<String>) {
    let current = version_id(&format!("current-{name}"), VERSION_ID_MIN_LEN);
    let created = ok(
        router,
        "CreateSecret",
        json!({
            "Name": name,
            "SecretString": "credential-0",
            "ClientRequestToken": current.clone(),
        }),
        case,
    )
    .await;
    let arn = text(&created, "ARN", case);
    match layout {
        Layout::OneVersion => (arn, current, None),
        Layout::CurrentAndPrevious => {
            let next = version_id(&format!("next-{name}"), VERSION_ID_MAX_LEN);
            ok(
                router,
                "PutSecretValue",
                json!({
                    "SecretId": name,
                    "SecretString": "credential-1",
                    "ClientRequestToken": next.clone(),
                }),
                case,
            )
            .await;
            (arn, next, None)
        }
        Layout::StalePendingCandidate => {
            // A rotation function's create step stores its candidate under `AWSPENDING`; a
            // finishing step that never ran leaves the label there instead of moving
            // `AWSCURRENT` onto it.
            let candidate = version_id(&format!("candidate-{name}"), VERSION_ID_MIN_LEN + 8);
            let staged = ok(
                router,
                "PutSecretValue",
                json!({
                    "SecretId": name,
                    "SecretString": "credential-candidate",
                    "ClientRequestToken": candidate.clone(),
                    "VersionStages": ["AWSPENDING"],
                }),
                case,
            )
            .await;
            assert_eq!(
                staged["VersionStages"],
                json!(["AWSPENDING"]),
                "case {case:?}: expected the candidate to carry only AWSPENDING, got {staged}"
            );
            (arn, current, Some(candidate))
        }
    }
}

#[tokio::test]
async fn an_absent_version_of_a_held_secret_is_refused_for_the_version() {
    let layouts = [
        ("one version holding AWSCURRENT", Layout::OneVersion),
        (
            "two versions, AWSCURRENT and AWSPREVIOUS",
            Layout::CurrentAndPrevious,
        ),
        (
            "a candidate left labelled AWSPENDING beside AWSCURRENT",
            Layout::StalePendingCandidate,
        ),
    ];
    let absent_ids = [
        (
            format!("a VersionId of the minimum modelled length ({VERSION_ID_MIN_LEN})"),
            version_id("absent-min", VERSION_ID_MIN_LEN),
        ),
        (
            "a VersionId in UUID form".to_string(),
            "11111111-2222-4333-8444-555555555555".to_string(),
        ),
        (
            format!("a VersionId of the maximum modelled length ({VERSION_ID_MAX_LEN})"),
            version_id("absent-max", VERSION_ID_MAX_LEN),
        ),
    ];

    for (index, (layout_name, layout)) in layouts.into_iter().enumerate() {
        for (id_name, absent) in &absent_ids {
            let case = format!("{id_name} read from a secret with {layout_name}");
            let router = emulator();
            let name = format!("not-found/classes-{index}");
            let (arn, readable, candidate) = secret_in(&router, &name, layout, &case).await;
            let before = staging_map(&router, &name, &case).await;

            let expected = expected_for_version_id(absent);
            // The secret resolves by either form of `SecretId`, so neither form turns a
            // missing VERSION into a missing SECRET.
            for secret_id in [name.clone(), arn.clone()] {
                let case = format!("{case}, naming the secret {secret_id:?}");
                let (status, code, message) = err(
                    &router,
                    "GetSecretValue",
                    json!({"SecretId": secret_id, "VersionId": absent}),
                    &case,
                )
                .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with message {message:?}"
                );
                assert_eq!(
                    code, "ResourceNotFoundException",
                    "case {case:?}: expected ResourceNotFoundException, got {code:?} with \
                     message {message:?}"
                );
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                // The secret itself WAS found, so the sentence for a missing secret is the
                // wrong answer here: the message is the discriminator between the classes.
                assert_ne!(
                    message, SECRET_NOT_FOUND,
                    "case {case:?}: expected the version sentence, got the missing-secret one"
                );
            }

            // The refused read resolved nothing and changed nothing: the versions the secret
            // does hold still read back, under the labels they had.
            let value = ok(
                &router,
                "GetSecretValue",
                json!({"SecretId": name.clone(), "VersionId": readable.clone()}),
                &case,
            )
            .await;
            assert_eq!(
                text(&value, "VersionId", &case),
                readable,
                "case {case:?}: expected the stored version readable, got {value}"
            );
            if let Some(candidate) = &candidate {
                let staged = ok(
                    &router,
                    "GetSecretValue",
                    json!({"SecretId": name.clone(), "VersionId": candidate}),
                    &case,
                )
                .await;
                assert_eq!(
                    staged["VersionStages"],
                    json!(["AWSPENDING"]),
                    "case {case:?}: expected the stale candidate still readable under \
                     AWSPENDING, got {staged}"
                );
                // "If the AWSPENDING staging label is present but not attached to the same
                // version as AWSCURRENT, then any later invocation of RotateSecret assumes
                // that a previous rotation request is still in progress and returns an error"
                // (`RotateSecret`, the Secrets Manager Smithy model) — so the stale candidate
                // is what a client has to notice, which is why the two not-found classes have
                // to be distinguishable.
                let (status, code, message) = err(
                    &router,
                    "RotateSecret",
                    json!({"SecretId": name.clone(), "RotationLambdaARN": LAMBDA_ARN}),
                    &case,
                )
                .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400 for a rotation over a stale candidate, \
                     got {status} with message {message:?}"
                );
                assert_eq!(
                    code, "InvalidRequestException",
                    "case {case:?}: expected InvalidRequestException, got {code:?} with \
                     message {message:?}"
                );
                // The message is the discriminator here too: this refusal states the rotation
                // already under way, not a resource that cannot be found.
                assert_eq!(
                    message, ROTATION_IN_PROGRESS_MESSAGE,
                    "case {case:?}: expected {ROTATION_IN_PROGRESS_MESSAGE:?}, got {message:?}"
                );
            }
            let after = staging_map(&router, &name, &case).await;
            assert_eq!(
                after, before,
                "case {case:?}: expected the staging labels unchanged by the refusals, got \
                 {after}"
            );
        }
    }
}

#[tokio::test]
async fn a_secret_the_store_does_not_hold_is_refused_for_the_secret() {
    // A `SecretId` that resolves to nothing is the OTHER class: the version the request named
    // is beside the point, because there is no secret to look for a version in. The rows run
    // over the form of the id (a plain name, a name with a path, a full ARN) and over which
    // version member — if any — the read asked for.
    let router = emulator();
    // A secret that DOES exist, so the store is not empty while the absent ids are read.
    secret_in(&router, "not-found/held", Layout::OneVersion, "setup").await;

    let secret_ids = [
        ("a plain name", "not-found-absent".to_string()),
        ("a name with a path", "team/app/absent-secret".to_string()),
        (
            "a full ARN",
            "arn:aws:secretsmanager:us-west-2:123456789012:secret:not-found/absent-AbCdEf"
                .to_string(),
        ),
    ];
    let selectors: [(&str, Value); 4] = [
        ("no version member at all", json!({})),
        (
            "a VersionId of the minimum modelled length",
            json!({"VersionId": version_id("ghost-min", VERSION_ID_MIN_LEN)}),
        ),
        (
            "a VersionId of the maximum modelled length",
            json!({"VersionId": version_id("ghost-max", VERSION_ID_MAX_LEN)}),
        ),
        (
            "the staging label AWSPENDING",
            json!({"VersionStage": "AWSPENDING"}),
        ),
    ];

    for (id_name, secret_id) in &secret_ids {
        for (selector_name, selector) in &selectors {
            let case = format!("{id_name} that names no secret, read with {selector_name}");
            let mut body = json!({"SecretId": secret_id});
            for (member, value) in selector.as_object().expect("the row states an object") {
                body[member] = value.clone();
            }
            let (status, code, message) = err(&router, "GetSecretValue", body.clone(), &case).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for {body}, got {status} with message \
                 {message:?}"
            );
            assert_eq!(
                code, "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException for {body}, got {code:?} \
                 with message {message:?}"
            );
            assert_eq!(
                message, SECRET_NOT_FOUND,
                "case {case:?}: expected {SECRET_NOT_FOUND:?}, got {message:?}"
            );
        }
    }
}
