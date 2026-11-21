// SPDX-License-Identifier: CC0-1.0

//! What a state machine whose DELETION HAS BEGUN answers, driven over this service's
//! own wire protocol through the production `MultiServiceRouter`.
//!
//! `DeleteStateMachine` "is an asynchronous operation. It sets the state machine's
//! status to DELETING and begins the deletion process", and "a state machine is deleted
//! only when all its executions are completed". The rules under test:
//!
//! 1. the deletion is in progress for as long as the state machine still runs an
//!    execution, so the record keeps reporting `DELETING` and every call that would
//!    start work on it — or mutate the versions and aliases it owns — keeps answering
//!    the refusal for that state, however many times a client asks;
//! 2. each refusal is worded for the operation it answers: `CreateStateMachineAlias`
//!    states what it could not do and the NAME of the state machine that stopped it,
//!    `RedriveExecution` states the execution it is about and then the status the state
//!    machine is in, and `PublishStateMachineVersion` carries the condition in its error
//!    code alone;
//! 3. a READ of what the state machine still holds is served throughout: its aliases are
//!    still there until the deletion is through, which is what a caller tearing a
//!    deployment down reads to find out what it still has — and once the last execution
//!    is done the record goes, after which every one of those calls reports the state
//!    machine as absent instead.
//!
//! The rows differ along the operation refused and along the class it belongs to (a
//! start, an alias mutation, a version mutation, a redrive, a create of the same name).
//! Every wait below is for a CONDITION — a status, a record going — and never for a span
//! of the clock.
//!
//! The harness (`Emulator`, `create_machine`, `string`, `error_code`) is the one
//! `state_machine_lifecycle` builds; `tests/all.rs` is this binary's crate root, so it
//! is reached through `crate::state_machine_lifecycle`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    alias_create_deleting_message, not_redrivable_message, NOT_REDRIVABLE_DELETING_REFUSED,
};

use crate::state_machine_lifecycle::{
    create_machine, error_code, string, Emulator, HELLO_DEFINITION, ROLE_ARN,
};

/// How many consecutive calls a client needs to answer the same way for the answer to
/// be one it can act on: it is the REPETITION that is asserted here, never a span of
/// the clock.
const CONSECUTIVE_CALLS: usize = 4;

/// A definition whose run is decided by its input: `{"hold":true}` waits `seconds`
/// before ending, and anything else fails at once — so one state machine can carry
/// both an execution that is still running and one that has failed.
fn held_or_failing_definition(seconds: u64) -> String {
    json!({
        "StartAt": "Pick",
        "States": {
            "Pick": {
                "Type": "Choice",
                "Choices": [{ "Variable": "$.hold", "BooleanEquals": true, "Next": "Hold" }],
                "Default": "Boom",
            },
            "Hold": { "Type": "Wait", "Seconds": seconds, "End": true },
            "Boom": { "Type": "Fail", "Error": "Boom", "Cause": "on purpose" },
        },
    })
    .to_string()
}

/// Start an execution of `machine` that holds for long enough that the deletion below
/// is in progress throughout, and answer its ARN.
async fn start_a_held_execution(emulator: &Emulator, machine: &str, name: &str) -> String {
    string(
        &emulator
            .ok(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": name,
                    "input": "{\"hold\":true}",
                }),
            )
            .await,
        "executionArn",
    )
}

/// Read `execution`'s status until it is terminal; the bound is generous on purpose,
/// because what is waited for is the STATUS and not a span.
async fn poll_until_terminal(emulator: &Emulator, execution: &str) {
    for _ in 0..10_000 {
        let described = emulator
            .ok("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if string(&described, "status") != "RUNNING" {
            return;
        }
    }
    panic!("case the failing execution: expected it to reach a terminal status");
}

/// Every call that starts work on a state machine whose deletion is in progress, and
/// every mutation of the versions and aliases it owns, is refused — and answers the
/// same refusal however many times a client asks.
#[tokio::test]
async fn a_deletion_in_progress_refuses_the_same_way_every_time() {
    let emulator = Emulator::new();
    let definition = held_or_failing_definition(120);
    let machine = create_machine(&emulator, "deleting-refusals", &definition).await;
    let version = string(
        &emulator
            .ok(
                "PublishStateMachineVersion",
                &json!({ "stateMachineArn": &machine }),
            )
            .await,
        "stateMachineVersionArn",
    );
    let express = string(
        &emulator
            .ok(
                "CreateStateMachine",
                &json!({
                    "name": "deleting-refusals-express",
                    "definition": &definition,
                    "roleArn": ROLE_ARN,
                    "type": "EXPRESS",
                }),
            )
            .await,
        "stateMachineArn",
    );
    // Each state machine is still RUNNING an execution, so its deletion is in progress
    // for as long as that execution is: "a state machine is deleted only when all its
    // executions are completed".
    start_a_held_execution(&emulator, &machine, "held").await;
    start_a_held_execution(&emulator, &express, "held").await;
    emulator
        .ok("DeleteStateMachine", &json!({ "stateMachineArn": &machine }))
        .await;
    emulator
        .ok("DeleteStateMachine", &json!({ "stateMachineArn": &express }))
        .await;

    // Each row: the class of call, the operation and the request.
    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "the asynchronous start of a Standard workflow",
            "StartExecution",
            json!({ "stateMachineArn": &machine, "input": "{}" }),
        ),
        (
            "the synchronous start of an Express workflow",
            "StartSyncExecution",
            json!({ "stateMachineArn": &express, "input": "{}" }),
        ),
        (
            "a create of an alias over its version",
            "CreateStateMachineAlias",
            json!({
                "name": "live",
                "routingConfiguration": [{ "stateMachineVersionArn": &version, "weight": 100 }],
            }),
        ),
        (
            "a publish of another version",
            "PublishStateMachineVersion",
            json!({ "stateMachineArn": &machine }),
        ),
        (
            "an update of its configuration",
            "UpdateStateMachine",
            json!({ "stateMachineArn": &machine, "definition": HELLO_DEFINITION }),
        ),
        (
            "a create of the same name again",
            "CreateStateMachine",
            json!({
                "name": "deleting-refusals",
                "definition": HELLO_DEFINITION,
                "roleArn": ROLE_ARN,
            }),
        ),
    ];

    for (case, operation, request) in &cases {
        for call in 0..CONSECUTIVE_CALLS {
            let (status, body) = emulator.call(operation, request).await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (400, "StateMachineDeleting"),
                "case {case:?}: call {call} must answer the same refusal, got {status} {code} \
                 with body {body:?}"
            );
        }
    }

    // And the record is what the refusals are about: it reports the status the delete
    // set, for as long as the deletion is in progress.
    let described = emulator
        .ok("DescribeStateMachine", &json!({ "stateMachineArn": &machine }))
        .await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("DELETING"),
        "the record reports the status the delete set, got {described:?}"
    );
}

/// Each refusal is worded for the operation it answers: the alias create names the
/// state machine it could not create the alias on, the redrive names the execution and
/// then the status, and the publish carries its code alone.
#[tokio::test]
async fn each_deleting_refusal_is_worded_for_its_own_operation() {
    let emulator = Emulator::new();
    let name = "deleting-wording";
    let machine = create_machine(&emulator, name, &held_or_failing_definition(120)).await;
    let version = string(
        &emulator
            .ok(
                "PublishStateMachineVersion",
                &json!({ "stateMachineArn": &machine }),
            )
            .await,
        "stateMachineVersionArn",
    );
    // One execution that FAILED, which the redrive below is about, and one that is
    // still running, which keeps the deletion in progress while these calls are made.
    let failed = string(
        &emulator
            .ok(
                "StartExecution",
                &json!({
                    "stateMachineArn": &machine,
                    "name": "failing",
                    "input": "{\"hold\":false}",
                }),
            )
            .await,
        "executionArn",
    );
    poll_until_terminal(&emulator, &failed).await;
    start_a_held_execution(&emulator, &machine, "held").await;
    emulator
        .ok("DeleteStateMachine", &json!({ "stateMachineArn": &machine }))
        .await;

    let (status, body) = emulator
        .call(
            "CreateStateMachineAlias",
            &json!({
                "name": "live",
                "routingConfiguration": [{ "stateMachineVersionArn": &version, "weight": 100 }],
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "StateMachineDeleting"),
        "an alias create on a deleting state machine is refused, got {status} with body {body:?}"
    );
    let expected = alias_create_deleting_message(name);
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(expected.as_str()),
        "the alias create states what it could not do and the state machine's name, expected \
         {expected:?}, got {body:?}"
    );

    let (status, body) = emulator
        .call("RedriveExecution", &json!({ "executionArn": &failed }))
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ExecutionNotRedrivable"),
        "a redrive on a deleting state machine is refused, got {status} with body {body:?}"
    );
    let expected = not_redrivable_message(&failed, NOT_REDRIVABLE_DELETING_REFUSED);
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(expected.as_str()),
        "the redrive names the execution and then the status, expected {expected:?}, got {body:?}"
    );
    assert!(
        expected.ends_with("State machine is DELETING"),
        "the reason is the status the state machine is in, got {expected:?}"
    );

    let (status, body) = emulator
        .call(
            "PublishStateMachineVersion",
            &json!({ "stateMachineArn": &machine }),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "StateMachineDeleting"),
        "a publish on a deleting state machine is refused, got {status} with body {body:?}"
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(""),
        "the publish carries the condition in its code and an empty message, got {body:?}"
    );
}

/// A READ of what the state machine still holds is served while the deletion is in
/// progress — and once its last execution is done the record goes, after which the same
/// calls report the state machine as absent.
#[tokio::test]
async fn a_deletion_in_progress_still_reports_what_it_holds_until_the_record_goes() {
    let emulator = Emulator::new();
    // The execution holds for ONE second, so the deletion goes through on its own: the
    // test then waits for the RECORD, never for a span.
    let machine = create_machine(&emulator, "deleting-reads", &held_or_failing_definition(1)).await;
    let version = string(
        &emulator
            .ok(
                "PublishStateMachineVersion",
                &json!({ "stateMachineArn": &machine }),
            )
            .await,
        "stateMachineVersionArn",
    );
    let alias = string(
        &emulator
            .ok(
                "CreateStateMachineAlias",
                &json!({
                    "name": "live",
                    "routingConfiguration": [
                        { "stateMachineVersionArn": &version, "weight": 100 },
                    ],
                }),
            )
            .await,
        "stateMachineAliasArn",
    );
    let held = start_a_held_execution(&emulator, &machine, "held").await;
    emulator
        .ok("DeleteStateMachine", &json!({ "stateMachineArn": &machine }))
        .await;

    // While the deletion is in progress the aliases are still there, and still
    // describe: the rows differ along which read is asked for.
    let listed = emulator
        .ok(
            "ListStateMachineAliases",
            &json!({ "stateMachineArn": &machine }),
        )
        .await;
    let aliases: Vec<String> = listed
        .get("stateMachineAliases")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|item| string(item, "stateMachineAliasArn"))
        .collect();
    assert_eq!(
        aliases,
        vec![alias.clone()],
        "a deleting state machine still lists the aliases it holds, got {listed:?}"
    );
    let read = emulator
        .ok(
            "DescribeStateMachineAlias",
            &json!({ "stateMachineAliasArn": &alias }),
        )
        .await;
    assert_eq!(
        string(&read, "stateMachineAliasArn"),
        alias,
        "the alias still describes itself, got {read:?}"
    );
    let described = emulator
        .ok("DescribeExecution", &json!({ "executionArn": &held }))
        .await;
    assert_eq!(
        string(&described, "status"),
        "RUNNING",
        "the execution the deletion waits for is still running, got {described:?}"
    );

    // Once the execution it was waiting for is done, the record goes — and from then on
    // the reads report the state machine as absent. The loop waits for the RECORD, with
    // a generous bound that only fails a store that never removes it.
    let mut gone = None;
    for _ in 0..10_000 {
        let (status, body) = emulator
            .call("DescribeStateMachine", &json!({ "stateMachineArn": &machine }))
            .await;
        if status != 200 {
            gone = Some((status, error_code(&body)));
            break;
        }
    }
    assert_eq!(
        gone.as_ref().map(|(status, code)| (*status, code.as_str())),
        Some((400, "StateMachineDoesNotExist")),
        "once the last execution is done the record must go, got {gone:?}"
    );
    let (status, body) = emulator
        .call(
            "ListStateMachineAliases",
            &json!({ "stateMachineArn": &machine }),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "StateMachineDoesNotExist"),
        "a removed state machine has no aliases to list, got {status} with body {body:?}"
    );
}
