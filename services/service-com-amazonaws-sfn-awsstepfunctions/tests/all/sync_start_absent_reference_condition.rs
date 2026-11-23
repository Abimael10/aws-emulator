// SPDX-License-Identifier: CC0-1.0

//! WHICH condition the SYNCHRONOUS start states for a state machine ARN this service
//! holds nothing for, driven over this service's own wire protocol (`awsJson1_0`,
//! `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`, so what is asserted is what a signed AWS SDK request observes.
//!
//! RULE under test: `StartSyncExecution` reports an ARN it can resolve no definition
//! for as the STATE MACHINE's own `StateMachineDoesNotExist` condition, whatever
//! qualifier the ARN carries — a version number, an alias name, or none at all. A
//! qualifier is a route to the definition this call runs rather than a resource the
//! call asked for, so a reference the synchronous start cannot run is the state machine
//! answering for nothing; the ARN is stated exactly as the request submitted it.
//!
//! The rows differ along what the rule generalises over: the FORM of the reference (a
//! version number never published, a version that was published and deleted, an alias
//! name no alias was created under, no qualifier at all), whether the state machine
//! behind the ARN exists, and the workflow TYPE behind it — the condition is the same
//! one every time.
//!
//! The ASYNCHRONOUS start keeps the condition of the resource its ARN refers to
//! (`tests/all/qualified_state_machine_arn_conditions.rs` owns that rule); the last
//! assertion here drives one dangling reference through both starts so the two
//! wordings are pinned against each other.
//!
//! The harness (`Emulator`, `create_machine`, `error_code`, `string`) is the one
//! `state_machine_lifecycle` builds; `tests/all/main.rs` is this binary's crate root,
//! so it is reached through `crate::state_machine_lifecycle`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    resource_error_message, STATE_MACHINE_DOES_NOT_EXIST, STATE_MACHINE_VERSION_DOES_NOT_EXIST,
};

use crate::state_machine_lifecycle::{
    create_machine, error_code, string, Emulator, ACCOUNT, HELLO_DEFINITION, REGION, ROLE_ARN,
};

/// A definition an EXPRESS workflow runs to a terminal status inside the one call.
const DONE: &str =
    r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","Result":{"ran":1},"End":true}}}"#;

/// Create an EXPRESS state machine, answering its ARN: the workflow type
/// `StartSyncExecution` serves ("StartSyncExecution is not available for STANDARD
/// workflows").
async fn express_machine(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .ok(
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": DONE,
                "roleArn": ROLE_ARN,
                "type": "EXPRESS",
            }),
        )
        .await;
    string(&created, "stateMachineArn")
}

/// Publish the state machine's current revision as its next version, answering that
/// version's ARN.
async fn publish(emulator: &Emulator, machine: &str) -> String {
    let published = emulator
        .ok(
            "PublishStateMachineVersion",
            &json!({ "stateMachineArn": machine }),
        )
        .await;
    string(&published, "stateMachineVersionArn")
}

/// The refusal one `StartSyncExecution` earns: `(status, error code, message)`.
async fn sync_start(emulator: &Emulator, arn: &str) -> (u16, String, Option<String>) {
    let (status, body) = emulator
        .call(
            "StartSyncExecution",
            &json!({ "stateMachineArn": arn, "input": "{}" }),
        )
        .await;
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string);
    (status, error_code(&body), message)
}

/// Every form of a reference the synchronous start can resolve nothing for earns the
/// state machine's own condition, stated over the ARN as submitted.
#[tokio::test]
async fn a_sync_start_through_a_reference_it_cannot_run_names_the_state_machine() {
    let emulator = Emulator::new();
    let express = express_machine(&emulator, "sync-dangling").await;
    let deleted_version = publish(&emulator, &express).await;
    emulator
        .ok(
            "DeleteStateMachineVersion",
            &json!({ "stateMachineVersionArn": &deleted_version }),
        )
        .await;
    // A STANDARD state machine too: the reference is resolved before the workflow type
    // is judged, so the type behind the ARN does not change the condition.
    let standard = create_machine(&emulator, "sync-dangling-standard", HELLO_DEFINITION).await;
    let absent = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:sync-never-created");

    // (case, the ARN submitted)
    let rows: Vec<(&str, String)> = vec![
        (
            "a version number never published on an EXPRESS machine",
            format!("{express}:4242"),
        ),
        (
            "a version that was published and then deleted",
            deleted_version.clone(),
        ),
        (
            "an alias name no alias was created under",
            format!("{express}:no-such-alias"),
        ),
        (
            "a version number never published on a STANDARD machine",
            format!("{standard}:7"),
        ),
        ("an unqualified ARN that names nothing", absent.clone()),
        ("a version of a state machine that names nothing", format!("{absent}:1")),
        (
            "an alias of a state machine that names nothing",
            format!("{absent}:live"),
        ),
    ];

    for (case, arn) in &rows {
        let (status, code, message) = sync_start(&emulator, arn).await;
        let expected = resource_error_message(STATE_MACHINE_DOES_NOT_EXIST, arn);
        assert_eq!(
            (status, code.as_str(), message.clone()),
            (400, "StateMachineDoesNotExist", Some(expected.clone())),
            "case {case:?}: expected 400 StateMachineDoesNotExist with {expected:?}, got {status} \
             {code} with {message:?}"
        );
    }
}

/// The references the synchronous start CAN run are still run, so the refusals above
/// are about the reference resolving to nothing and not about a qualified ARN.
#[tokio::test]
async fn the_references_a_sync_start_can_run_are_still_run() {
    let emulator = Emulator::new();
    let express = express_machine(&emulator, "sync-resolvable").await;
    let version = publish(&emulator, &express).await;
    let alias = string(
        &emulator
            .ok(
                "CreateStateMachineAlias",
                &json!({
                    "name": "LIVE",
                    "routingConfiguration": [
                        { "stateMachineVersionArn": &version, "weight": 100 },
                    ],
                }),
            )
            .await,
        "stateMachineAliasArn",
    );

    for (case, arn) in [
        ("the unqualified ARN", express.clone()),
        ("the published version's ARN", version.clone()),
        ("an alias routing to that version", alias.clone()),
    ] {
        let answered = emulator
            .ok(
                "StartSyncExecution",
                &json!({ "stateMachineArn": &arn, "input": "{}" }),
            )
            .await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the run the reference resolves to, got {answered:?}"
        );
    }
}

/// The two starts word the SAME dangling version differently: the synchronous one
/// states the state machine's condition, the asynchronous one the version's.
#[tokio::test]
async fn the_two_starts_word_the_same_dangling_version_differently() {
    let emulator = Emulator::new();
    let express = express_machine(&emulator, "sync-versus-async").await;
    let dangling = format!("{express}:9");

    let (status, code, message) = sync_start(&emulator, &dangling).await;
    let expected = resource_error_message(STATE_MACHINE_DOES_NOT_EXIST, &dangling);
    assert_eq!(
        (status, code.as_str(), message.clone()),
        (400, "StateMachineDoesNotExist", Some(expected.clone())),
        "case the synchronous start: expected {expected:?}, got {status} {code} with {message:?}"
    );

    let (status, body) = emulator
        .call(
            "StartExecution",
            &json!({ "stateMachineArn": &dangling, "input": "{}" }),
        )
        .await;
    let expected = resource_error_message(STATE_MACHINE_VERSION_DOES_NOT_EXIST, &dangling);
    assert_eq!(
        (status, body.get("message").and_then(Value::as_str)),
        (400, Some(expected.as_str())),
        "case the asynchronous start: expected {expected:?}, got {status} with body {body:?}"
    );
}
