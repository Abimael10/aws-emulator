// SPDX-License-Identifier: CC0-1.0

//! Two REFUSALS this service words from the run it is about, driven over this service's
//! own wire protocol (`awsJson1_0`) through the production `MultiServiceRouter`.
//!
//! RULE 1: `StartSyncExecution`'s `error` is the member a synchronous caller reads an
//! unsuccessful run's reason off, so a run the state machine's own `TimeoutSeconds`
//! ended answers the Amazon States Language's own timeout name there — an asynchronous
//! execution carries the status alone, because its caller reads the timeout off the
//! status and the run's history instead.
//!
//! RULE 2: the two places this service reports an execution's ineligibility for a
//! redrive word the history-event limit differently: `redriveStatusReason` carries the
//! sentence that member's own documentation lists, and `ExecutionNotRedrivable` states
//! the limit the execution went past. The sentence is asserted here as the TEXT it must
//! be rather than by calling the function that builds it, so a change to either wording
//! is caught.
//!
//! The harness (`Emulator`, `string`) is `state_machine_lifecycle`'s; `tests/all/main.rs`
//! declares every suite of this target, so it is reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    not_redrivable_message, NOT_REDRIVABLE_EVENT_LIMIT,
};

use crate::state_machine_lifecycle::{string, Emulator, ROLE_ARN};

/// The ASL error name a run the machine's own `TimeoutSeconds` ended reports.
const TIMEOUT_ERROR: &str = "States.Timeout";

/// A synchronous EXPRESS run the state machine's own `TimeoutSeconds` ends answers
/// `TIMED_OUT` with that timeout as the `error` of the response itself.
#[tokio::test]
async fn a_synchronous_run_the_machine_timeout_ends_answers_the_timeout_as_its_error() {
    // (case, the definition, the status the response must carry, the error it must
    //  carry)
    let rows: Vec<(&str, Value, &str, Option<&str>)> = vec![
        (
            "a machine whose timeout is shorter than its own Wait",
            json!({
                "TimeoutSeconds": 1,
                "StartAt": "Hold",
                "States": {
                    "Hold": { "Type": "Wait", "Seconds": 30, "Next": "Done" },
                    "Done": { "Type": "Pass", "End": true },
                },
            }),
            "TIMED_OUT",
            Some(TIMEOUT_ERROR),
        ),
        (
            // The control: the same machine without the timeout succeeds, so the row
            // above reports the timeout and not merely every unsuccessful run.
            "the same machine with no timeout at all",
            json!({
                "StartAt": "Hold",
                "States": {
                    "Hold": { "Type": "Wait", "Seconds": 30, "Next": "Done" },
                    "Done": { "Type": "Pass", "End": true },
                },
            }),
            "SUCCEEDED",
            None,
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition, status, error)) in rows.into_iter().enumerate() {
        let created = emulator
            .ok(
                "CreateStateMachine",
                &json!({
                    "name": format!("sync-timeout-{index}"),
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                    "type": "EXPRESS",
                }),
            )
            .await;
        let machine = string(&created, "stateMachineArn");
        let answered = emulator
            .ok(
                "StartSyncExecution",
                &json!({ "stateMachineArn": machine, "name": "only", "input": "{}" }),
            )
            .await;
        assert_eq!(
            string(&answered, "status"),
            status,
            "case {case:?}: expected the run to answer {status}, got {answered:?}"
        );
        assert_eq!(
            answered.get("error").and_then(Value::as_str),
            error,
            "case {case:?}: expected the error {error:?}, got {answered:?}"
        );
    }
}

/// The refusal a redrive of an execution past the history-event limit carries states
/// the limit the execution went past, and names the execution it is about.
#[test]
fn the_redrive_refusal_states_the_history_limit_the_execution_went_past() {
    const EXECUTION: &str = "arn:aws:states:us-west-2:000000000000:execution:sm:run";
    let case = "an execution past the history-event limit";
    let message = not_redrivable_message(EXECUTION, NOT_REDRIVABLE_EVENT_LIMIT);
    assert_eq!(
        message,
        format!(
            "Execution {EXECUTION} invalid for redrive as: Execution has exceeded the maximum \
             history event limit"
        ),
        "case {case:?}: expected the refusal to state the limit it went past, got {message:?}"
    );
    // And the member that reports the same ineligibility keeps the sentence its own
    // documentation lists, which is a different one.
    assert_eq!(
        NOT_REDRIVABLE_EVENT_LIMIT, "Execution history event limit exceeded",
        "case {case:?}: expected the member's own documented sentence"
    );
}
