// SPDX-License-Identifier: CC0-1.0

//! What becomes of the JOB when the bound its task declared runs out, driven over this
//! service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: a run-a-job (`.sync`) task that runs out of the span its state
//! declared fails its execution with `States.Timeout`, and the job it started is STOPPED
//! — nothing waits at it any more, so it must not carry on to an outcome of its own. That
//! closure is the CLOCK's: it falls due at the instant the bound is reached whether or not
//! anybody calls, and it is materialised by whichever caller reads the store first — the
//! watcher that polls the job, a report of the parent execution, or any other request of
//! this service. The job is stopped either way, so a client that reads the parent's
//! failure and then the job finds the job stopped rather than still running.
//!
//! The rows span what the rule generalises over: WHICH bound declares the span — the
//! state's `TimeoutSeconds`, its `HeartbeatSeconds`, or both with the heartbeat the
//! shorter of the two — and each row has a report of the parent materialise the closure,
//! which is what a client waiting for the parent's outcome does.
//!
//! The integration driven here is the nested execution
//! (`arn:aws:states:::states:startExecution.sync`), whose `StartExecution`,
//! `DescribeExecution` and `StopExecution` this service serves itself, so the job's own
//! status is read back through this API.
//!
//! The harness (`Emulator`, `create_machine`, `string`) is the one
//! `state_machine_lifecycle` builds, and the job helpers are `sync_service_integration`'s;
//! every suite of this crate is a module of the one test binary, so each is reached
//! through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{create_machine, string, Emulator};
use crate::sync_service_integration::{
    described, submitted_job, CHILD_RUNS_ON, PATIENCE,
};

/// The ASL error name a task that ran out of the time it was allowed reports: "if the
/// task runs longer than the specified seconds, this state fails with a States.Timeout
/// error name", which `HeartbeatSeconds` closes its window with too.
const TASK_TIMEOUT: &str = "States.Timeout";

/// The span every row bounds its task by: the shortest a state may declare, since both
/// members are "a positive integer" of SECONDS.
const BOUND_SECONDS: u64 = 1;

/// A parent definition whose one `Task` state starts `child_arn` through the run-a-job
/// pattern, bounded by the members `bounds` names.
///
/// The `Parameters` document IS the request of the call the integration makes
/// (`StartExecution`: `stateMachineArn`, `input`), so its members are the ones that
/// operation's own model declares.
fn bounded_parent(child_arn: &str, bounds: &[(&str, u64)]) -> String {
    let mut task = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::states:startExecution.sync",
        "Parameters": { "stateMachineArn": child_arn, "input": "{}" },
        "End": true,
    });
    let members = task.as_object_mut().expect("the state is an object");
    for (member, seconds) in bounds {
        members.insert((*member).to_string(), json!(seconds));
    }
    json!({ "StartAt": "RunChild", "States": { "RunChild": task } }).to_string()
}

/// The execution `execution_arn` names once it reports `status`, or a panic naming the
/// case when it never does.
///
/// The wait is for the CONDITION, under a deadline generous enough to fail only an
/// emulator that never reaches it: the bound the rows declare is the clock's, and how
/// long this host takes to carry it through is not what is under test.
async fn awaiting(emulator: &Emulator, case: &str, execution_arn: &str, status: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = described(emulator, execution_arn).await;
        if described.get("status").and_then(Value::as_str) == Some(status) {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected {execution_arn} to report {status} within {PATIENCE:?}, got \
             {described:?}"
        );
        ::tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One row: the case, and the bound members the parent's `Task` state declares.
type BoundRow = (&'static str, &'static [(&'static str, u64)]);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_whose_task_ran_out_of_its_bound_is_stopped() {
    let rows: [BoundRow; 3] = [
        (
            "a task bounded by TimeoutSeconds",
            &[("TimeoutSeconds", BOUND_SECONDS)],
        ),
        (
            "a task bounded by HeartbeatSeconds",
            &[("HeartbeatSeconds", BOUND_SECONDS)],
        ),
        (
            "a task whose HeartbeatSeconds is the shorter of its two bounds",
            &[
                ("HeartbeatSeconds", BOUND_SECONDS),
                ("TimeoutSeconds", BOUND_SECONDS * 600),
            ],
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, bounds)) in rows.into_iter().enumerate() {
        // The child keeps running far past every bound the parent declares, so what it
        // ends as is decided by the stop rather than by work of its own.
        let child_arn = create_machine(&emulator, &format!("held-child-{index}"), CHILD_RUNS_ON).await;
        let parent_arn = create_machine(
            &emulator,
            &format!("bounded-parent-{index}"),
            &bounded_parent(&child_arn, bounds),
        )
        .await;
        let started = emulator
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": parent_arn, "name": format!("bounded-{index}") }),
            )
            .await;
        let parent_execution = string(&started, "executionArn");

        // The job really started: it is a child execution of its own, running.
        let job = submitted_job(&emulator, case, &parent_execution).await;
        let running = described(&emulator, &job).await;
        assert_eq!(
            running.get("status").and_then(Value::as_str),
            Some("RUNNING"),
            "case {case:?}: expected the job to be running, got {running:?}"
        );

        // A client waiting for the parent's outcome polls it, and that report is what
        // materialises the closure the clock owes: the parent fails on the bound.
        let failed = awaiting(&emulator, case, &parent_execution, "FAILED").await;
        assert_eq!(
            failed.get("error").and_then(Value::as_str),
            Some(TASK_TIMEOUT),
            "case {case:?}: expected the bound's own error name, got {failed:?}"
        );

        // And the job nothing waits at any more is STOPPED, not left to run on: it ends
        // as a stopped execution does, with no outcome of its own.
        let stopped = awaiting(&emulator, case, &job, "ABORTED").await;
        assert_eq!(
            stopped.get("output").and_then(Value::as_str),
            None,
            "case {case:?}: a stopped job reports no output of its own, got {stopped:?}"
        );
        assert!(
            stopped.get("stopDate").is_some(),
            "case {case:?}: expected the job to report when it was stopped, got {stopped:?}"
        );
    }
}
