// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the JOB a run-a-job (`.sync`) `Task` state leaves behind when
//! the task it belongs to is CLOSED while that job is still running, driven over this
//! service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! RULE: a job started for a task nothing waits for any more is STOPPED, through the
//! stopping call its own integration names — however the task was closed, and whichever
//! caller materialised that closure. The closure is the clock's or a caller's, not the
//! job poll's, so the stop follows from the closure itself rather than from which caller
//! happened to be looking when it came due.
//!
//! The rows differ along what the rule generalises over: WHICH closure closed the task
//! (the span the `Task` state allowed its own work, the execution's own allowance, a
//! caller's `StopExecution`), which member declared that span, and how long it was. The
//! integration both halves use is Step Functions' own
//! (`arn:aws:states:::states:startExecution.sync`), so the job the task started is a
//! child execution really started here, and whether it was stopped is read off that
//! child's own status through this API.
//!
//! The harness (`Emulator`, `create_machine`, `string`) is `state_machine_lifecycle`'s,
//! the child definition and the terminal-status reader are `sync_service_integration`'s
//! and the child-execution reader is `integration_task_heartbeat`'s;
//! `tests/all/main.rs` declares every suite of this target, so each is reached through
//! `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EXECUTION_STATUS_ABORTED, EXECUTION_STATUS_FAILED, EXECUTION_STATUS_TIMED_OUT,
    TASK_TIMEOUT_ERROR,
};

use crate::integration_task_heartbeat::child_execution;
use crate::state_machine_lifecycle::{create_machine, string, Emulator};
use crate::sync_service_integration::{settled, CHILD_RUNS_ON};

/// How the row's parent task is closed while the job it started is still running.
#[derive(Debug, Clone, Copy)]
enum Closure {
    /// The clock ran out the span the `Task` state allowed its own work.
    TaskBound,
    /// The clock ran out the EXECUTION's own allowance while it was parked at the job.
    ExecutionAllowance,
    /// A caller stopped the execution that was parked at the job.
    Caller,
}

/// One row of the table: the case, the bounds the parent's `Task` state declares, the
/// `TimeoutSeconds` of the parent DEFINITION (the execution's own allowance), what
/// closes the task, and what the parent must then report.
struct Row {
    case: &'static str,
    bounds: Value,
    allowance: Option<u64>,
    closure: Closure,
    status: &'static str,
    error: Option<&'static str>,
}

/// A parent definition whose one `Task` state starts `child` through the run-a-job
/// pattern, carrying whatever `bounds` declares — and, where `allowance` is given, the
/// definition's own `TimeoutSeconds` around it.
///
/// Built here rather than taken from a sibling suite because the execution's own
/// allowance is a member of the DEFINITION rather than of the state, which is one of the
/// closures this suite varies over.
fn parent(child: &str, bounds: &Value, allowance: Option<u64>) -> String {
    let mut state = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::states:startExecution.sync",
        "Parameters": { "stateMachineArn": child, "input": "{}" },
        "End": true,
    });
    let members = state.as_object_mut().expect("the state is an object");
    for (member, value) in bounds.as_object().expect("the bounds are an object") {
        members.insert(member.clone(), value.clone());
    }
    let mut definition = json!({ "StartAt": "RunChild", "States": { "RunChild": state } });
    if let Some(seconds) = allowance {
        definition
            .as_object_mut()
            .expect("the definition is an object")
            .insert("TimeoutSeconds".to_string(), Value::from(seconds));
    }
    definition.to_string()
}

/// A job whose task was closed while it was still running is STOPPED: the child
/// execution the task started ends `ABORTED` rather than running on behind it, however
/// the task was closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_task_stops_the_job_it_started() {
    let rows: Vec<Row> = vec![
        Row {
            case: "the heartbeat window the state declares, its only bound",
            bounds: json!({ "HeartbeatSeconds": 1 }),
            allowance: None,
            closure: Closure::TaskBound,
            status: EXECUTION_STATUS_FAILED,
            error: Some(TASK_TIMEOUT_ERROR),
        },
        Row {
            case: "the timeout the state declares, its only bound",
            bounds: json!({ "TimeoutSeconds": 1 }),
            allowance: None,
            closure: Closure::TaskBound,
            status: EXECUTION_STATUS_FAILED,
            error: Some(TASK_TIMEOUT_ERROR),
        },
        Row {
            case: "the heartbeat window, three orders of magnitude inside the timeout",
            bounds: json!({ "HeartbeatSeconds": 1, "TimeoutSeconds": 3600 }),
            allowance: None,
            closure: Closure::TaskBound,
            status: EXECUTION_STATUS_FAILED,
            error: Some(TASK_TIMEOUT_ERROR),
        },
        Row {
            case: "a timeout of two seconds rather than one",
            bounds: json!({ "TimeoutSeconds": 2 }),
            allowance: None,
            closure: Closure::TaskBound,
            status: EXECUTION_STATUS_FAILED,
            error: Some(TASK_TIMEOUT_ERROR),
        },
        Row {
            case: "the execution's own allowance, the state declaring no bound at all",
            bounds: json!({}),
            allowance: Some(1),
            closure: Closure::ExecutionAllowance,
            status: EXECUTION_STATUS_TIMED_OUT,
            error: None,
        },
        Row {
            case: "a caller stopping the execution parked at the job",
            bounds: json!({}),
            allowance: None,
            closure: Closure::Caller,
            status: EXECUTION_STATUS_ABORTED,
            error: None,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        // One emulator per row, so the child executions a row reads are its own.
        let emulator = Emulator::new();
        let child_machine = create_machine(&emulator, "held-child", CHILD_RUNS_ON).await;
        let definition = parent(&child_machine, &row.bounds, row.allowance);
        let machine =
            create_machine(&emulator, &format!("held-parent-{index}"), &definition).await;
        let started = emulator
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "only", "input": "{}" }),
            )
            .await;
        let execution = string(&started, "executionArn");

        // The job really started: the task's work was performed, so there is a child
        // execution for the closure below to leave running or stop.
        let child = child_execution(&emulator, case, &child_machine).await;

        if matches!(row.closure, Closure::Caller) {
            emulator
                .ok("StopExecution", &json!({ "executionArn": execution }))
                .await;
        }

        let parent_record = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                string(&parent_record, "status"),
                parent_record.get("error").and_then(Value::as_str),
            ),
            (row.status.to_string(), row.error),
            "case {case:?}: expected the closure to end the parent, got {parent_record:?}"
        );

        let child_record = settled(&emulator, case, &child).await;
        assert_eq!(
            string(&child_record, "status"),
            EXECUTION_STATUS_ABORTED,
            "case {case:?}: expected the job nobody waits for to be stopped, got \
             {child_record:?}"
        );
    }
}
