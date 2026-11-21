// SPDX-License-Identifier: CC0-1.0

//! What an `encryptionConfiguration` naming a customer managed KMS key the account
//! does not hold answers, driven over both services' own wire protocols through the
//! production `MultiServiceRouter` with Step Functions and KMS mounted on ONE engine —
//! so the key a configuration names is the key KMS's own handlers hold.
//!
//! The rule under test: a configuration of `CUSTOMER_MANAGED_KMS_KEY` commits the
//! resource's data to the key it names, so the service can apply it only while that key
//! is one KMS resolves the reference to. A reference that resolves to no key leaves
//! nothing to mint the resource's data keys with, which is the modelled
//! `InvalidEncryptionConfiguration` — carrying the sentence the lookup came back with,
//! relayed — rather than a create that is accepted and fails at the first write.
//!
//! The rows differ along the operation the configuration is handed to
//! (`CreateActivity`, `CreateStateMachine`, `UpdateStateMachine`), along the FORM of
//! the reference (a key ARN, a bare key id, an alias, an alias ARN) and along whether
//! KMS holds the key it names — and each refused call is followed by a read proving
//! nothing was stored.
//!
//! The harness (`Emulator`, its `kms_key`/`kms_alias` helpers, `string`, `error_code`)
//! is the one `state_machine_lifecycle` builds; `tests/all.rs` is this binary's crate
//! root, so it is reached through `crate::state_machine_lifecycle`.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, error_code, string, Emulator, ACCOUNT, HELLO_DEFINITION, REGION, ROLE_ARN,
};

/// The customer-managed encryption configuration naming `key`.
fn customer_managed(key: &str) -> Value {
    json!({ "type": "CUSTOMER_MANAGED_KMS_KEY", "kmsKeyId": key })
}

/// The references KMS resolves to no key of this account in this Region, one per FORM
/// the `kmsKeyId` member documents ("an alias, alias ARN, key ID, or key ARN").
fn absent_references() -> Vec<(&'static str, String)> {
    vec![
        (
            "a key ARN of a key that was never created",
            format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-3333-4444-555555555555"),
        ),
        (
            "a bare key id of a key that was never created",
            "22222222-3333-4444-5555-666666666666".to_string(),
        ),
        (
            "an alias no CreateAlias ever created",
            "alias/never-created".to_string(),
        ),
        (
            "an alias ARN no CreateAlias ever created",
            format!("arn:aws:kms:{REGION}:{ACCOUNT}:alias/never-created"),
        ),
    ]
}

/// A create that names a key KMS holds nothing for is refused, whichever operation
/// takes the configuration and whichever form named the key — and stores nothing.
#[tokio::test]
async fn a_configuration_on_an_absent_key_is_refused_by_every_operation() {
    let emulator = Emulator::new();
    // The state machine the update rows configure, which has to exist first.
    let machine = create_machine(&emulator, "absent-key-updates", HELLO_DEFINITION).await;

    for (index, (case, reference)) in absent_references().into_iter().enumerate() {
        // Each row: the operation, the request it is refused for, and the read that
        // proves the refusal stored nothing.
        let activity = format!("absent-key-activity-{index}");
        let created = format!("absent-key-machine-{index}");
        let calls: Vec<(&str, Value)> = vec![
            (
                "CreateActivity",
                json!({
                    "name": &activity,
                    "encryptionConfiguration": customer_managed(&reference),
                }),
            ),
            (
                "CreateStateMachine",
                json!({
                    "name": &created,
                    "definition": HELLO_DEFINITION,
                    "roleArn": ROLE_ARN,
                    "encryptionConfiguration": customer_managed(&reference),
                }),
            ),
            (
                "UpdateStateMachine",
                json!({
                    "stateMachineArn": &machine,
                    // Named beside the configuration under test: an update must carry
                    // at least one of the members its own rule admits, which an
                    // `encryptionConfiguration` is not one of.
                    "definition": HELLO_DEFINITION,
                    "encryptionConfiguration": customer_managed(&reference),
                }),
            ),
        ];
        for (operation, request) in calls {
            let (status, body) = emulator.call(operation, &request).await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidEncryptionConfiguration"),
                "case {case:?} / {operation}: a key KMS holds nothing for must be refused, got \
                 {status} {code} with body {body:?}"
            );
            // The refusal RELAYS the lookup: it names the reference the configuration
            // carried rather than wording a condition of this service's own.
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                message.contains(&reference),
                "case {case:?} / {operation}: the refusal must name the reference it could not \
                 resolve, got {message:?}"
            );
        }

        // Nothing was stored, and the state machine the update named is untouched: it
        // still reports the Amazon Web Services owned key.
        let (status, body) = emulator
            .call(
                "DescribeActivity",
                &json!({
                    "activityArn": format!("arn:aws:states:{REGION}:{ACCOUNT}:activity:{activity}"),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ActivityDoesNotExist"),
            "case {case:?}: a refused create must store no activity, got {status} with body \
             {body:?}"
        );
        let (status, body) = emulator
            .call(
                "DescribeStateMachine",
                &json!({
                    "stateMachineArn":
                        format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{created}"),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "StateMachineDoesNotExist"),
            "case {case:?}: a refused create must store no state machine, got {status} with body \
             {body:?}"
        );
        let described = emulator
            .ok("DescribeStateMachine", &json!({ "stateMachineArn": &machine }))
            .await;
        assert_eq!(
            described
                .get("encryptionConfiguration")
                .and_then(|configuration| configuration.get("type"))
                .and_then(Value::as_str),
            Some("AWS_OWNED_KEY"),
            "case {case:?}: a refused update must leave the configuration alone, got {described:?}"
        );
    }
}

/// And a configuration naming a key KMS DOES hold is applied, by every one of those
/// operations and through every reference form — which is what makes the refusal above
/// about the key's absence rather than about the configuration's shape.
#[tokio::test]
async fn a_configuration_on_a_key_that_exists_is_applied() {
    let emulator = Emulator::new();
    let key = emulator.kms_key().await;
    let key_id = key.rsplit('/').next().unwrap_or_default().to_string();
    emulator.kms_alias("alias/applied", &key).await;

    // The rows differ along the FORM the configuration names the key by; what the
    // record keeps is the key itself, so each of them reads back as the key's ARN.
    let references: Vec<(&str, String)> = vec![
        ("the key ARN", key.clone()),
        ("the bare key id", key_id),
        ("an alias of the key", "alias/applied".to_string()),
        (
            "the alias ARN of the key",
            format!("arn:aws:kms:{REGION}:{ACCOUNT}:alias/applied"),
        ),
    ];
    for (index, (case, reference)) in references.into_iter().enumerate() {
        let activity = emulator
            .ok(
                "CreateActivity",
                &json!({
                    "name": format!("present-key-activity-{index}"),
                    "encryptionConfiguration": customer_managed(&reference),
                }),
            )
            .await;
        let described = emulator
            .ok(
                "DescribeActivity",
                &json!({ "activityArn": string(&activity, "activityArn") }),
            )
            .await;
        assert_eq!(
            described
                .get("encryptionConfiguration")
                .and_then(|configuration| configuration.get("kmsKeyId"))
                .and_then(Value::as_str),
            Some(key.as_str()),
            "case {case:?}: the activity must be stored on the key the reference named, got \
             {described:?}"
        );

        let machine = emulator
            .ok(
                "CreateStateMachine",
                &json!({
                    "name": format!("present-key-machine-{index}"),
                    "definition": HELLO_DEFINITION,
                    "roleArn": ROLE_ARN,
                    "encryptionConfiguration": customer_managed(&reference),
                }),
            )
            .await;
        let arn = string(&machine, "stateMachineArn");
        let described = emulator
            .ok("DescribeStateMachine", &json!({ "stateMachineArn": &arn }))
            .await;
        assert_eq!(
            described
                .get("encryptionConfiguration")
                .and_then(|configuration| configuration.get("kmsKeyId"))
                .and_then(Value::as_str),
            Some(key.as_str()),
            "case {case:?}: the state machine must be stored on that key, got {described:?}"
        );
    }
}
