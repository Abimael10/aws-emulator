// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `HeartbeatSeconds` on a `Task` state whose `Resource` names a
//! SERVICE INTEGRATION, driven over this service's own wire protocol (`awsJson1_0`,
//! `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`. The integration both halves use is Step Functions' own
//! (`states:startExecution`, and its run-a-job revision `states:startExecution.sync`),
//! so the task's work is really performed and the child execution it starts is really
//! there to read.
//!
//! RULE: `HeartbeatSeconds`/`HeartbeatSecondsPath` is a `Task` state's member whatever
//! work its `Resource` names. The heartbeats the member's window is measured between are
//! sent by whoever is DOING the work, so for a task whose work this emulator performs
//! itself nothing heartbeats it and the window closes at its own length: such a task is
//! held to whichever of the two spans the state declared runs out first, while each
//! member is still REPORTED as the definition wrote it. When that bound closes a
//! run-a-job task, the job it started is waited for by nobody and is STOPPED — the child
//! execution ends `ABORTED` rather than running on behind the task it failed.
//!
//! The rows differ along what the rule generalises over: which of the two members the
//! state declares, and which of them is the shorter — including the case where the
//! heartbeat is the shorter by three orders of magnitude, which a task bounded by its
//! `TimeoutSeconds` alone could not end inside this suite's patience.
//!
//! The harness (`Emulator`, `create_machine`, `string`) is `state_machine_lifecycle`'s
//! and the history reader is `execution_history`'s; `tests/all/main.rs` declares every
//! suite of this target, so both are reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_TASK_SCHEDULED, EXECUTION_STATUS_ABORTED,
};

use crate::execution_history::history;
use crate::state_machine_lifecycle::{create_machine, string, Emulator};

/// The ASL error name a task whose own clock closed reports.
const TIMEOUT_ERROR: &str = "States.Timeout";

/// How long a row waits for an execution to leave `RUNNING`: generous, because what it
/// waits for is this emulator's own poll interval and settle delay rather than the
/// host's speed.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long a row waits between two looks at a status.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A child that finishes at once.
const CHILD_FINISHES: &str =
    r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","Result":{"ok":true},"End":true}}}"#;

/// A child that keeps running: its one `Wait` is owed an hour, so it is `RUNNING` for as
/// long as a row needs it to be — and is still `RUNNING` when its parent's own bound
/// closes.
const CHILD_RUNS_ON: &str = r#"{"StartAt":"Hold","States":{"Hold":{"Type":"Wait",
    "Seconds":3600,"Next":"Done"},"Done":{"Type":"Pass","End":true}}}"#;

/// A parent whose one `Task` state starts `child` through the pattern `suffix` names,
/// carrying whatever bounds `bounds` declares.
fn parent(child: &str, suffix: &str, bounds: &Value) -> String {
    let mut state = json!({
        "Type": "Task",
        "Resource": format!("arn:aws:states:::states:startExecution{suffix}"),
        "Parameters": { "stateMachineArn": child, "input": "{}" },
        "End": true,
    });
    let members = state.as_object_mut().expect("the state is an object");
    for (member, value) in bounds.as_object().expect("the bounds are an object") {
        members.insert(member.clone(), value.clone());
    }
    json!({ "StartAt": "RunChild", "States": { "RunChild": state } }).to_string()
}

/// Start an execution of `machine` that must succeed, and answer its ARN.
async fn start(emulator: &Emulator, machine: &str, name: &str) -> String {
    let started = emulator
        .ok(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": name, "input": "{}" }),
        )
        .await;
    string(&started, "executionArn")
}

/// `DescribeExecution`, as the API answers it.
async fn described(emulator: &Emulator, execution: &str) -> Value {
    emulator
        .ok("DescribeExecution", &json!({ "executionArn": execution }))
        .await
}

/// The record of `execution` once it has left `RUNNING`, or a panic naming `case`.
async fn settled(emulator: &Emulator, case: &str, execution: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = described(emulator, execution).await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected {execution} to reach a terminal status within {PATIENCE:?}, \
             got {described:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The child execution the parent's one child-start answered, read off the executions
/// of the child state machine rather than guessed at.
async fn child_execution(emulator: &Emulator, case: &str, child_machine: &str) -> String {
    let began = Instant::now();
    loop {
        let listed = emulator
            .ok(
                "ListExecutions",
                &json!({ "stateMachineArn": child_machine }),
            )
            .await;
        let executions = listed
            .get("executions")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected an executions list in {listed:?}"));
        if let Some(first) = executions.first() {
            return string(first, "executionArn");
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the parent to have started a child within {PATIENCE:?}, got \
             {listed:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// The member is accepted, and reported as the definition wrote it
// ---------------------------------------------------------------------------

/// One row of the reporting table: the case, the bounds the state declares, and the two
/// figures the event the task is scheduled by must carry.
struct BoundsRow {
    case: &'static str,
    bounds: Value,
    timeout: Option<i64>,
    heartbeat: Option<i64>,
}

/// A service-integration `Task` carrying `HeartbeatSeconds` RUNS — the member is a
/// `Task` state's whatever work its `Resource` names — and the event the task is
/// scheduled by reports both bounds exactly as the state declared them.
#[tokio::test]
async fn an_integration_task_carrying_a_heartbeat_runs_and_reports_both_bounds() {
    let rows: Vec<BoundsRow> = vec![
        BoundsRow {
            case: "a heartbeat alone",
            bounds: json!({ "HeartbeatSeconds": 60 }),
            timeout: None,
            heartbeat: Some(60),
        },
        BoundsRow {
            case: "a heartbeat shorter than the timeout",
            bounds: json!({ "HeartbeatSeconds": 60, "TimeoutSeconds": 300 }),
            timeout: Some(300),
            heartbeat: Some(60),
        },
        BoundsRow {
            case: "a timeout shorter than the heartbeat",
            bounds: json!({ "HeartbeatSeconds": 300, "TimeoutSeconds": 60 }),
            timeout: Some(60),
            heartbeat: Some(300),
        },
        BoundsRow {
            case: "a timeout alone, which the member's absence leaves as the only bound",
            bounds: json!({ "TimeoutSeconds": 120 }),
            timeout: Some(120),
            heartbeat: None,
        },
        BoundsRow {
            case: "neither bound",
            bounds: json!({}),
            timeout: None,
            heartbeat: None,
        },
    ];

    let emulator = Emulator::new();
    let child = create_machine(&emulator, "heartbeat-child", CHILD_FINISHES).await;
    for (
        index,
        BoundsRow {
            case,
            bounds,
            timeout,
            heartbeat,
        },
    ) in rows.into_iter().enumerate()
    {
        let definition = parent(&child, "", &bounds);
        let machine =
            create_machine(&emulator, &format!("heartbeat-parent-{index}"), &definition).await;
        let execution = start(&emulator, &machine, "only").await;
        let settled = settled(&emulator, case, &execution).await;
        assert_eq!(
            string(&settled, "status"),
            "SUCCEEDED",
            "case {case:?}: a task carrying the member runs, got {settled:?}"
        );

        let events = history(&emulator, &execution, json!({})).await;
        let scheduled = events
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some(EVENT_TASK_SCHEDULED))
            .unwrap_or_else(|| panic!("case {case:?}: expected a {EVENT_TASK_SCHEDULED} event"));
        let details = scheduled
            .get("taskScheduledEventDetails")
            .unwrap_or_else(|| panic!("case {case:?}: expected the details in {scheduled:?}"));
        assert_eq!(
            (
                details.get("timeoutInSeconds").and_then(Value::as_i64),
                details.get("heartbeatInSeconds").and_then(Value::as_i64),
            ),
            (timeout, heartbeat),
            "case {case:?}: expected the bounds the state declared, got {details:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The shorter of the two bounds closes the task, and stops the job
// ---------------------------------------------------------------------------

/// A run-a-job task is held to whichever of the two spans the state declared runs out
/// first, and the job it started is STOPPED when that span closes: the child execution
/// ends `ABORTED` rather than running on.
///
/// The second row is what pins the `min` rather than the timeout alone: a task bounded
/// by its `TimeoutSeconds` of an hour could not end inside this suite's patience, so the
/// row can only pass if the heartbeat window decided.
#[tokio::test]
async fn the_shorter_bound_closes_a_run_a_job_task_and_stops_its_child() {
    // (case, the bounds the state declares)
    let rows: Vec<(&str, Value)> = vec![
        ("a heartbeat alone", json!({ "HeartbeatSeconds": 1 })),
        (
            "a heartbeat far shorter than the timeout",
            json!({ "HeartbeatSeconds": 1, "TimeoutSeconds": 3600 }),
        ),
        (
            "a timeout shorter than the heartbeat",
            json!({ "HeartbeatSeconds": 3600, "TimeoutSeconds": 1 }),
        ),
    ];

    for (index, (case, bounds)) in rows.into_iter().enumerate() {
        // One emulator per row, so the child executions a row reads are its own.
        let emulator = Emulator::new();
        let child_machine = create_machine(&emulator, "held-child", CHILD_RUNS_ON).await;
        let definition = parent(&child_machine, ".sync", &bounds);
        let machine =
            create_machine(&emulator, &format!("held-parent-{index}"), &definition).await;
        let execution = start(&emulator, &machine, "only").await;

        // The child really started: the task's work was performed before its own clock
        // closed it.
        let child = child_execution(&emulator, case, &child_machine).await;

        let parent_record = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                string(&parent_record, "status"),
                parent_record.get("error").and_then(Value::as_str),
            ),
            ("FAILED".to_string(), Some(TIMEOUT_ERROR)),
            "case {case:?}: expected the task's own clock to close it, got {parent_record:?}"
        );

        let child_record = settled(&emulator, case, &child).await;
        assert_eq!(
            string(&child_record, "status"),
            EXECUTION_STATUS_ABORTED,
            "case {case:?}: expected the job nobody waits for to be stopped, got {child_record:?}"
        );
    }
}
