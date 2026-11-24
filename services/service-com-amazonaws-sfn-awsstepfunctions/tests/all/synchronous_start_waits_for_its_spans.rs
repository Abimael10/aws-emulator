// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a synchronous Express start answers, driven over this
//! service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.StartSyncExecution`) through the production
//! `MultiServiceRouter`.
//!
//! RULE: `StartSyncExecution` answers the run's own TERMINAL outcome — "StartSyncExecution
//! will return a 200 OK response, even if your execution fails" — so it does not answer
//! before the instant the spans the definition declares make that outcome due. A caller
//! therefore never reads a `stopDate` the clock has not reached: the instant the response
//! reports the run as having stopped at is at or before the instant the caller observes
//! the answer. The one bound on that is the longest a run of this workflow type can last
//! (`state::MAX_SYNCHRONOUS_RUN_DURATION`), which every row below is far inside.
//!
//! The rows differ along what the rule generalises over: WHICH declared span decides the
//! instant (a machine-level `TimeoutSeconds` shorter than the run's own `Wait`, a `Wait`
//! the run gets past, two `Wait` states that add up, and a definition declaring no span at
//! all), and therefore along the terminal status the run reaches (`TIMED_OUT` and
//! `SUCCEEDED`).
//!
//! The assertion is an ORDERING of instants the service itself reports and the caller
//! observes, never a measured duration: a slower host only makes the observed instant
//! later, so no row turns on how fast this host runs.
//!
//! The harness (`Emulator`, `string`, `ROLE_ARN`) is the one `state_machine_lifecycle`
//! builds; `tests/all/main.rs` declares every suite of this target, so it is reached
//! through `crate::state_machine_lifecycle`.

use std::time::SystemTime;

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{string, Emulator, ROLE_ARN};

/// The ASL error name a run the machine's own `TimeoutSeconds` ended reports.
const TIMEOUT_ERROR: &str = "States.Timeout";

/// One row: the case, the definition, and the terminal status the run must report.
struct Row {
    case: &'static str,
    definition: Value,
    status: &'static str,
    error: Option<&'static str>,
}

/// A `Wait` state of `seconds`, going on to `next`.
fn waits(seconds: u64, next: &str) -> Value {
    json!({ "Type": "Wait", "Seconds": seconds, "Next": next })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The machine's own bound is shorter than the span its definition declares, so
            // the run ends at the bound rather than at the `Wait`.
            case: "a TimeoutSeconds shorter than the run's own Wait",
            definition: json!({
                "TimeoutSeconds": 1,
                "StartAt": "Hold",
                "States": {
                    "Hold": waits(30, "Done"),
                    "Done": { "Type": "Pass", "End": true },
                },
            }),
            status: "TIMED_OUT",
            error: Some(TIMEOUT_ERROR),
        },
        Row {
            case: "a Wait the run gets past, so the run succeeds at that instant",
            definition: json!({
                "StartAt": "Hold",
                "States": {
                    "Hold": waits(1, "Done"),
                    "Done": { "Type": "Pass", "Result": { "msg": "hi" }, "End": true },
                },
            }),
            status: "SUCCEEDED",
            error: None,
        },
        Row {
            case: "two Waits, whose spans add up",
            definition: json!({
                "StartAt": "First",
                "States": {
                    "First": waits(1, "Second"),
                    "Second": waits(1, "Done"),
                    "Done": { "Type": "Pass", "Result": { "msg": "hi" }, "End": true },
                },
            }),
            status: "SUCCEEDED",
            error: None,
        },
        Row {
            // The control: a definition declaring no span at all answers at once, and the
            // same ordering holds.
            case: "no declared span at all",
            definition: json!({
                "StartAt": "Done",
                "States": { "Done": { "Type": "Pass", "Result": { "msg": "hi" }, "End": true } },
            }),
            status: "SUCCEEDED",
            error: None,
        },
    ]
}

/// The instant `member` of `answered` names, as whole and fractional epoch seconds.
fn instant(case: &str, answered: &Value, member: &str) -> f64 {
    answered
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member}, got {answered:?}"))
}

/// This process's own clock, as the epoch seconds the response's instants are reported in.
fn now() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// A synchronous Express start does not answer before the instant the run's own declared
/// spans make its outcome due.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_synchronous_start_answers_no_sooner_than_the_run_stopped() {
    let emulator = Emulator::new();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let created = emulator
            .ok(
                "CreateStateMachine",
                &json!({
                    "name": format!("sync-span-{index}"),
                    "definition": row.definition.to_string(),
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
        // Read AFTER the call came back, so the comparison below is "the run had already
        // stopped when the caller was answered".
        let observed = now();

        assert_eq!(
            string(&answered, "status"),
            row.status,
            "case {case:?}: expected the run to answer {}, got {answered:?}",
            row.status
        );
        assert_eq!(
            answered.get("error").and_then(Value::as_str),
            row.error,
            "case {case:?}: expected the error {:?}, got {answered:?}",
            row.error
        );
        let start_date = instant(case, &answered, "startDate");
        let stop_date = instant(case, &answered, "stopDate");
        assert!(
            stop_date >= start_date,
            "case {case:?}: expected the run to stop no earlier than it started, got \
             startDate {start_date} and stopDate {stop_date}"
        );
        assert!(
            stop_date <= observed,
            "case {case:?}: expected the call to answer no sooner than the stopDate it \
             reports, got stopDate {stop_date} answered by {observed}"
        );
    }
}
