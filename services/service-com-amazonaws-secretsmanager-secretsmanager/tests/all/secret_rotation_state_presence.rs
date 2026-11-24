// SPDX-License-Identifier: CC0-1.0

//! `RotationEnabled` is reported only once a call has DECIDED a secret's rotation.
//!
//! The rule under test: `RotationEnabled` is "whether automatic rotation is turned on for this
//! secret" (`DescribeSecretResponse$RotationEnabled`, this service's Smithy model), a boolean
//! the model gives no default — so the member is absent from the body of a secret rotation was
//! never configured for, exactly as the rest of the rotation view (`RotationLambdaARN`,
//! `RotationRules`, `LastRotatedDate`, `NextRotationDate`) is, and it appears once
//! a call the model documents as DECIDING rotation has been made: true once `RotateSecret`
//! turned rotation on, false once `CancelRotateSecret` turned it off ("To turn on rotation,
//! use RotateSecret. To turn off rotation, use CancelRotateSecret"). Its presence is what
//! tells a never-configured secret apart from one rotation was turned off for, so neither a
//! read nor a write that was not asked to decide rotation may materialise it.
//!
//! The rule holds of every projection of a secret that carries the member, so the tables run
//! over `DescribeSecret` and the `SecretListEntry` of `ListSecrets`, over the calls that write
//! a secret without deciding its rotation (`PutSecretValue`, `UpdateSecret`,
//! `TagResource`, `DeleteSecret`), over repeated reads of the same secret, and over each way
//! rotation gets turned on and then off again.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 header naming `secretsmanager` in `us-west-2`; the router routes on the credential
/// scope and the emulator verifies signatures only when an authenticator is registered, which
/// this harness does not do.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-west-2/secretsmanager/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

/// The ARN of a Lambda rotation function, as `RotationLambdaARN` carries it.
const LAMBDA_ARN: &str = "arn:aws:lambda:us-west-2:123456789012:function:RotateMySecret";

/// Every member of the rotation view that `DescribeSecret` and `ListSecrets` carry, which a
/// secret rotation was never configured for reports none of.
const ROTATION_MEMBERS: [&str; 5] = [
    "RotationEnabled",
    "RotationLambdaARN",
    "RotationRules",
    "LastRotatedDate",
    "NextRotationDate",
];

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

/// What `DescribeSecret` reports for `secret_id`.
async fn describe(router: &MultiServiceRouter, secret_id: &str, case: &str) -> Value {
    ok(router, "DescribeSecret", json!({"SecretId": secret_id}), case).await
}

/// The `SecretListEntry` `ListSecrets` reports for the secret named `name`. A secret inside
/// its recovery window is listed only when the caller asks for it, which
/// `IncludePlannedDeletion` does, so every row is looked up the same way.
async fn list_entry(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let listed = ok(
        router,
        "ListSecrets",
        json!({
            "IncludePlannedDeletion": true,
            "Filters": [{"Key": "name", "Values": [name]}],
        }),
        case,
    )
    .await;
    listed
        .get("SecretList")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no entry for {name} in {listed}"))
}

/// A version id (and so a `ClientRequestToken`) of the modelled minimum length (`@length`
/// 32..64, this service's Smithy model), unique per `seed`.
fn version_id(seed: &str) -> String {
    let mut id = format!("v-{seed}-");
    while id.len() < 32 {
        id.push('z');
    }
    id
}

#[tokio::test]
async fn a_secret_rotation_was_never_configured_for_reports_no_rotation_view() {
    let router = emulator();

    // (case, the write performed on the secret after it was created)
    let writes: Vec<(&str, Option<(&str, Value)>)> = vec![
        ("nothing but the create", None),
        (
            "a second value put on it",
            Some((
                "PutSecretValue",
                json!({"SecretString": "value-1", "ClientRequestToken": version_id("put")}),
            )),
        ),
        (
            "its description updated",
            Some(("UpdateSecret", json!({"Description": "described"}))),
        ),
        (
            "a tag written on it",
            Some(("TagResource", json!({"Tags": [{"Key": "team", "Value": "a"}]}))),
        ),
        // A delete turns the schedule of a rotating secret off, but it was not asked to
        // decide rotation: there is nothing to turn off on a secret rotation was never
        // configured for, and the secret still exists inside its recovery window to be
        // described.
        (
            "scheduled for deletion, having never been configured for rotation",
            Some(("DeleteSecret", json!({"RecoveryWindowInDays": 30}))),
        ),
    ];

    for (index, (case, write)) in writes.iter().enumerate() {
        let name = format!("never-rotated-{index}");
        ok(
            &router,
            "CreateSecret",
            json!({
                "Name": &name,
                "SecretString": "value-0",
                "ClientRequestToken": version_id(&format!("create-{index}")),
            }),
            case,
        )
        .await;
        if let Some((operation, members)) = write {
            let mut body = json!({"SecretId": &name});
            for (member, value) in members.as_object().expect("the row states an object") {
                body[member] = value.clone();
            }
            ok(&router, operation, body, case).await;
        }

        // Repeated reads answer the same: a read configures nothing, so no number of
        // describes materialises the member.
        for pass in 1..=3 {
            let described = describe(&router, &name, case).await;
            for member in ROTATION_MEMBERS {
                assert_eq!(
                    described.get(member),
                    None,
                    "case {case:?} (describe pass {pass}): expected no {member} on a secret \
                     rotation was never configured for, got {described}"
                );
            }
        }

        // And the same secret in the list projection.
        let entry = list_entry(&router, &name, case).await;
        for member in ROTATION_MEMBERS {
            assert_eq!(
                entry.get(member),
                None,
                "case {case:?}: expected no {member} in the SecretListEntry of a secret \
                 rotation was never configured for, got {entry}"
            );
        }
    }
}

/// One call a row drives the secret through: the operation and the members it carries beside
/// `SecretId`.
type Call = (&'static str, Value);

#[tokio::test]
async fn configuring_rotation_makes_the_member_appear_and_turning_it_off_keeps_it() {
    // (case, the calls made after the create, the RotationEnabled expected afterwards)
    let rows: Vec<(&str, Vec<Call>, bool)> = vec![
        (
            "rotation turned on by RotateSecret",
            vec![(
                "RotateSecret",
                json!({
                    "RotationLambdaARN": LAMBDA_ARN,
                    "RotationRules": {"AutomaticallyAfterDays": 10},
                    "RotateImmediately": false,
                }),
            )],
            true,
        ),
        (
            "rotation turned off again by CancelRotateSecret",
            vec![
                (
                    "RotateSecret",
                    json!({
                        "RotationLambdaARN": LAMBDA_ARN,
                        "RotationRules": {"ScheduleExpression": "rate(12 hours)"},
                        "RotateImmediately": false,
                    }),
                ),
                ("CancelRotateSecret", json!({})),
            ],
            false,
        ),
        // "To turn off rotation, use CancelRotateSecret": the call decides the flag, so it has
        // a value to report even for a secret `RotateSecret` never configured rotation for.
        (
            "rotation turned off by CancelRotateSecret, having never been configured",
            vec![("CancelRotateSecret", json!({}))],
            false,
        ),
        (
            "rotation turned off by scheduling the deletion of a rotating secret",
            vec![
                (
                    "RotateSecret",
                    json!({
                        "RotationLambdaARN": LAMBDA_ARN,
                        "RotationRules": {"AutomaticallyAfterDays": 30},
                        "RotateImmediately": false,
                    }),
                ),
                ("DeleteSecret", json!({"RecoveryWindowInDays": 7})),
            ],
            false,
        ),
        (
            "rotation left off by the restore of a deleted rotating secret",
            vec![
                (
                    "RotateSecret",
                    json!({
                        "RotationLambdaARN": LAMBDA_ARN,
                        "RotationRules": {"AutomaticallyAfterDays": 30},
                        "RotateImmediately": false,
                    }),
                ),
                ("DeleteSecret", json!({"RecoveryWindowInDays": 7})),
                ("RestoreSecret", json!({})),
            ],
            false,
        ),
    ];

    for (index, (case, calls, expected)) in rows.iter().enumerate() {
        let router = emulator();
        let name = format!("configured-{index}");
        ok(
            &router,
            "CreateSecret",
            json!({
                "Name": &name,
                "SecretString": "value-0",
                "ClientRequestToken": version_id(&format!("configured-{index}")),
            }),
            case,
        )
        .await;
        // Before any of those calls the member is absent, which is what makes its presence
        // afterwards the signal the rule is about.
        let before = describe(&router, &name, case).await;
        assert_eq!(
            before.get("RotationEnabled"),
            None,
            "case {case:?}: expected no RotationEnabled before rotation was configured, got \
             {before}"
        );

        for (operation, members) in calls {
            let mut body = json!({"SecretId": &name});
            for (member, value) in members.as_object().expect("the row states an object") {
                body[member] = value.clone();
            }
            ok(&router, operation, body, case).await;
        }

        let described = describe(&router, &name, case).await;
        assert_eq!(
            described.get("RotationEnabled"),
            Some(&json!(expected)),
            "case {case:?}: expected RotationEnabled {expected}, got {described}"
        );
    }
}
