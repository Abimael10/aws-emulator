// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a `Task` state inside a `Parallel` BRANCH whose answer comes
//! from OUTSIDE the execution, driven over this service's own wire protocol
//! (`awsJson1_0`, `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the
//! production `MultiServiceRouter`.
//!
//! RULE: a `Task` state whose work is answered from outside the execution may sit inside a
//! `Parallel` branch — a branch reports nothing until it ends, so the branch WAITS: the
//! task is queued for a worker to claim, the execution stays `RUNNING`, and the answer
//! resumes the execution inside that branch, with the branches that had already finished
//! keeping what they produced. A `Catch` the waiting state declares answers for the task's
//! failure exactly as it answers for a failure of work the walk performed itself, so a
//! `Parallel` that declares no `Catch` of its own still SUCCEEDS.
//!
//! The rows differ along what the rule generalises over: HOW the task is answered (a
//! worker's `SendTaskSuccess`, a worker's `SendTaskFailure` the state's own `Catch` takes
//! over), WHICH branch waits (the first of two, and the second — so the record the
//! suspension keeps names a branch other than zero and the other branch's result has to
//! survive the suspension), and HOW DEEP the branch is nested (one `Parallel`, and a
//! `Parallel` inside a `Parallel`).
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds, and the activity helpers
//! (`activity_arn`, `create_activity`) are `activity_tasks`'; `tests/all/main.rs` declares
//! every suite of this target, so both are reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::activity_tasks::{activity_arn, create_activity};
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// How long a poll waits for the status the clock owes before giving up.
///
/// Generous: what the loop waits for is the CONDITION, so no row turns on how fast this
/// host runs.
const PATIENCE: Duration = Duration::from_secs(90);

/// How long one poll pauses between two looks.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// The error a worker reports for the row whose failure the state's own `Catch` takes
/// over, and the name that catcher is written against.
const REPORTED_ERROR: &str = "WorkerSaysNo";

/// The result the branch that does NOT wait produces, so a row can show that what a
/// finished branch produced survives the suspension.
const FAST_RESULT: &str = "fast";

/// How a row's waiting task is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// `SendTaskSuccess`: "the JSON output of the task" is the state's result.
    Success,
    /// `SendTaskFailure` naming [`REPORTED_ERROR`], which the state's own `Catch` takes
    /// over.
    CaughtFailure,
}

/// One row: the case, where in the definition the waiting task sits, how it is answered,
/// and what the execution must end with.
struct Row {
    case: &'static str,
    /// Which branch of the outer `Parallel` the waiting task sits in.
    waiting_branch: usize,
    /// How many `Parallel` levels the waiting state is nested inside.
    levels: usize,
    answer: Answer,
    /// The whole output the execution must end with.
    output: Value,
}

/// The state machine one branch of a row runs: a `Task` that waits for `activity` with a
/// `Catch` of its own naming [`REPORTED_ERROR`], and the state that catcher carries the
/// execution on at.
fn waiting_machine(activity: &str) -> Value {
    json!({
        "StartAt": "Waits",
        "States": {
            "Waits": {
                "Type": "Task",
                "Resource": activity,
                "Catch": [{
                    "ErrorEquals": [REPORTED_ERROR],
                    "ResultPath": "$.caught",
                    "Next": "Recovered",
                }],
                "End": true,
            },
            "Recovered": { "Type": "Pass", "Result": { "recovered": true }, "End": true },
        },
    })
}

/// A branch that finishes at once, so a row can show that the branch which did NOT wait
/// keeps what it produced.
fn fast_machine() -> Value {
    json!({
        "StartAt": "Fast",
        "States": { "Fast": { "Type": "Pass", "Result": FAST_RESULT, "End": true } },
    })
}

/// The branch the waiting task sits in, `levels` deep: the waiting machine itself at one
/// level, and that machine inside a `Parallel` of its own at two.
///
/// The inner `Parallel`'s `OutputPath` unwraps its one-branch array, so the document the
/// branch produces is the same at either depth and a row's expected output does not have
/// to restate the nesting. Its state name differs from the outer one's, because "state
/// names must be unique within the scope of the whole state machine".
fn waiting_branch(activity: &str, levels: usize) -> Value {
    let machine = waiting_machine(activity);
    if levels <= 1 {
        return machine;
    }
    json!({
        "StartAt": "Nested",
        "States": {
            "Nested": {
                "Type": "Parallel",
                "Branches": [machine],
                "OutputPath": "$[0]",
                "End": true,
            }
        },
    })
}

/// The definition a row runs: an outer `Parallel` over two branches, the waiting one at
/// `branch`.
///
/// Neither `Parallel` declares a `Catch`, which is what makes a row that ends SUCCEEDED
/// the waiting state's own `Catch` doing the work.
fn definition(activity: &str, branch: usize, levels: usize) -> String {
    let waiting = waiting_branch(activity, levels);
    let branches: Vec<Value> = match branch {
        0 => vec![waiting, fast_machine()],
        _ => vec![fast_machine(), waiting],
    };
    json!({
        "StartAt": "Both",
        "States": {
            "Both": { "Type": "Parallel", "Branches": branches, "End": true }
        },
    })
    .to_string()
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the first branch of two waits and the worker reports a result",
            waiting_branch: 0,
            levels: 1,
            answer: Answer::Success,
            output: json!([{ "done": true }, FAST_RESULT]),
        },
        Row {
            case: "the SECOND branch of two waits and the worker reports a result",
            waiting_branch: 1,
            levels: 1,
            answer: Answer::Success,
            output: json!([FAST_RESULT, { "done": true }]),
        },
        Row {
            case: "the waiting state's own Catch absorbs the failure a worker reported",
            waiting_branch: 0,
            levels: 1,
            answer: Answer::CaughtFailure,
            output: json!([{ "recovered": true }, FAST_RESULT]),
        },
        Row {
            case: "a Parallel inside a Parallel, two levels down",
            waiting_branch: 1,
            levels: 2,
            answer: Answer::Success,
            output: json!([FAST_RESULT, { "done": true }]),
        },
    ]
}

/// One execution waits at ONE unit of work whose answer comes from outside it: a second
/// concurrent suspension in a sibling branch of the same `Parallel` is REFUSED, in either
/// order, so no record is left naming a branch its own state does not live in.
///
/// The rows differ along which kind of work suspends FIRST — a `Parallel` whose branch 0
/// schedules an activity task and whose branch 1 starts a run-a-job (`.sync`) child, and
/// the same pair the other way round.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_concurrent_suspension_in_a_sibling_branch_is_refused() {
    let emulator = Emulator::new();
    // The child a `.sync` task would start: it keeps running, so a job that WAS started
    // would park rather than finish inside the call.
    let child = create_machine(
        &emulator,
        "sibling-child",
        r#"{"StartAt":"Hold","States":{"Hold":{"Type":"Wait","Seconds":3600,"End":true}}}"#,
    )
    .await;

    // (case, whether the activity branch comes first)
    let rows: [(&str, bool); 2] = [
        ("the activity task is reached first", true),
        ("the run-a-job child is reached first", false),
    ];
    for (index, (case, activity_first)) in rows.into_iter().enumerate() {
        let name = format!("two-suspensions-{index}");
        let activity = create_activity(&emulator, &name).await;
        let waits = waiting_machine(&activity);
        let starts_a_job = json!({
            "StartAt": "Child",
            "States": {
                "Child": {
                    "Type": "Task",
                    "Resource": "arn:aws:states:::states:startExecution.sync",
                    "Parameters": { "stateMachineArn": child, "input": "{}" },
                    "End": true,
                }
            },
        });
        let branches: Vec<Value> = match activity_first {
            true => vec![waits, starts_a_job],
            false => vec![starts_a_job, waits],
        };
        let definition = json!({
            "StartAt": "Both",
            "States": {
                "Both": { "Type": "Parallel", "Branches": branches, "End": true }
            },
        })
        .to_string();
        let machine = create_machine(&emulator, &name, &definition).await;
        let execution = start_execution(&emulator, &machine, "only", "{}").await;

        let ended = reported_as(&emulator, case, &execution, "FAILED").await;
        assert_eq!(
            ended.get("error").and_then(Value::as_str),
            Some("States.Runtime"),
            "case {case:?}: expected the second suspension to fail the execution, got {ended:?}"
        );
        let cause = ended
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            cause.contains("already waiting at") && cause.contains("nothing was run for it"),
            "case {case:?}: expected the cause to say the second suspension was not run, got \
             {cause:?}"
        );
    }
}

/// Claim the task `activity` has for a worker: `GetActivityTask` is itself a long poll, so
/// one call waits for the task to arrive and answers `(the token, the input)`.
async fn claim(emulator: &Emulator, case: &str, activity: &str) -> (String, Value) {
    let answered = emulator
        .ok("GetActivityTask", &json!({ "activityArn": activity }))
        .await;
    let token = answered
        .get("taskToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a task on {activity}, got {answered:?}")
        })
        .to_string();
    let input: Value = serde_json::from_str(&string(&answered, "input")).unwrap_or_else(|why| {
        panic!("case {case:?}: expected JSON input on the task, got {why} for {answered:?}")
    });
    (token, input)
}

/// Describe `execution` until it reports `status`, and answer what it then said.
async fn reported_as(emulator: &Emulator, case: &str, execution: &str, status: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let described = describe_execution(emulator, execution).await;
        if described.get("status").and_then(Value::as_str) == Some(status) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {execution} to report {status}, got {described:?}"
        );
        ::tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// A `Task` inside a `Parallel` branch WAITS for its worker, and the answer resumes the
/// execution inside that branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parallel_branch_waits_for_the_task_one_of_its_states_scheduled() {
    let emulator = Emulator::new();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("branch-wait-{index}");
        let activity = create_activity(&emulator, &name).await;
        assert_eq!(
            activity,
            activity_arn(&name),
            "case {case:?}: expected the activity's own ARN, got {activity:?}"
        );
        let machine = create_machine(
            &emulator,
            &name,
            &definition(&activity, row.waiting_branch, row.levels),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", "{}").await;

        // The task really was scheduled under the activity, from inside the branch: a
        // worker claims it, which is only possible because the branch waited instead of
        // the state failing for having nowhere to wait.
        let (token, input) = claim(&emulator, case, &activity).await;
        assert_eq!(
            input,
            json!({}),
            "case {case:?}: expected the state's effective input on the task, got {input:?}"
        );

        // And while it is outstanding the execution has reached no outcome at all.
        let waiting = describe_execution(&emulator, &execution).await;
        assert_eq!(
            waiting.get("status").and_then(Value::as_str),
            Some("RUNNING"),
            "case {case:?}: expected the execution to wait at the task, got {waiting:?}"
        );

        match row.answer {
            Answer::Success => {
                emulator
                    .ok(
                        "SendTaskSuccess",
                        &json!({
                            "taskToken": token,
                            "output": json!({ "done": true }).to_string(),
                        }),
                    )
                    .await;
            }
            Answer::CaughtFailure => {
                emulator
                    .ok(
                        "SendTaskFailure",
                        &json!({ "taskToken": token, "error": REPORTED_ERROR }),
                    )
                    .await;
            }
        }

        let ended = reported_as(&emulator, case, &execution, "SUCCEEDED").await;
        let output: Value =
            serde_json::from_str(&string(&ended, "output")).unwrap_or_else(|why| {
                panic!("case {case:?}: expected JSON output, got {why} for {ended:?}")
            });
        assert_eq!(
            output, row.output,
            "case {case:?}: expected the Parallel to put out {}, got {output:?}",
            row.output
        );
        assert!(
            ended.get("error").is_none_or(Value::is_null),
            "case {case:?}: a Parallel whose branch's Catch absorbed the failure reports no \
             error, got {ended:?}"
        );
    }
}

/// The resumed execution appends to the ONE history: the branch's task is reported as
/// scheduled once and answered once, and the `Parallel` the walk re-entered is not
/// entered a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_resumed_history_reports_the_branch_task_once() {
    let emulator = Emulator::new();
    let case = "a branch task answered by a worker";
    let activity = create_activity(&emulator, "branch-history").await;
    let machine =
        create_machine(&emulator, "branch-history", &definition(&activity, 0, 1)).await;
    let execution = start_execution(&emulator, &machine, "only", "{}").await;
    let (token, _input) = claim(&emulator, case, &activity).await;
    emulator
        .ok(
            "SendTaskSuccess",
            &json!({ "taskToken": token, "output": json!({ "done": true }).to_string() }),
        )
        .await;
    reported_as(&emulator, case, &execution, "SUCCEEDED").await;

    let history = emulator
        .ok(
            "GetExecutionHistory",
            &json!({ "executionArn": execution, "maxResults": 200 }),
        )
        .await;
    let events = history
        .get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a history, got {history:?}"))
        .clone();
    let types: Vec<&str> = events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str))
        .collect();
    for required in [
        "ParallelStateEntered",
        "ActivityScheduled",
        "ActivitySucceeded",
        "ParallelStateExited",
        "ExecutionSucceeded",
    ] {
        assert!(
            types.contains(&required),
            "case {case:?}: expected a {required} event, got {types:?}"
        );
    }
    let counted = |kind: &str| types.iter().filter(|seen| **seen == kind).count();
    assert_eq!(
        counted("ActivityScheduled"),
        1,
        "case {case:?}: expected one ActivityScheduled event, got {types:?}"
    );
    assert_eq!(
        counted("ParallelStateEntered"),
        1,
        "case {case:?}: expected one ParallelStateEntered event, got {types:?}"
    );
}
