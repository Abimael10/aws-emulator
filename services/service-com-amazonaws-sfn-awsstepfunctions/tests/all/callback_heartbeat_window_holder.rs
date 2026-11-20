// SPDX-License-Identifier: CC0-1.0

//! What a report of progress does to the window a callback (`.waitForTaskToken`) task's
//! `HeartbeatSeconds` opens, with the HOLDER of the token being this suite itself: Step
//! Functions and Amazon SQS over one engine, the token read back out of the queue the
//! task's own integration call sent it to, and every `SendTaskHeartbeat` made from here.
//!
//! RULE: "this action resets the Heartbeat clock" (`SendTaskHeartbeat`), and
//! "HeartbeatSeconds […] if more time than the specified seconds elapses between heartbeats
//! from the task, this state fails with a States.Timeout error name". The member therefore
//! bounds the IDLE span between two reports of progress and not the span the work takes:
//! every report on a task's token starts the window again, so work that keeps reporting
//! progress inside it settles its task however many windows' worth of work it does, and
//! only a window nothing reported progress inside closes the task — with `States.Timeout`
//! and the work family's own timeout event.
//!
//! The holder is the test, so every span the rule turns on is one this process measures and
//! spends itself: nothing is unpacked, no interpreter is started, and each report of
//! progress is answered in process. That is what this suite adds next to
//! [`crate::callback_heartbeat_window`], which drives the same rule over a real Lambda
//! worker holding the token, and next to the suites that redeem a token out of a queue
//! WITHOUT spending real time between reports
//! ([`crate::callback_service_integration`]): what is new here is idle spans that are a
//! fraction of the window over work that outlasts two of them, which is the suspended
//! callback task's own window being started again and again.
//!
//! The rows differ along what the rule generalises over: the LENGTH of the window (two,
//! four and five seconds), how often progress is reported inside it (a fifth of the window,
//! a quarter of it, never), and how many windows' worth of work the holder does before
//! settling the task (none, more than two of them either way).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{asl::TIMEOUT_ERROR, EVENT_TASK_TIMED_OUT};

use crate::callback_timeout_error_handling::{count_of, Emulator};
use crate::state_machine_lifecycle::string;

/// The state whose callback task every row below waits at.
const WAITING_STATE: &str = "WaitForHolder";

/// The queue each row's callback integration hands its token to: one per row, named after
/// it.
const QUEUE_PREFIX: &str = "heartbeat-window-holder";

/// The statuses an execution of these machines is read in (`ExecutionStatus`).
const RUNNING: &str = "RUNNING";
/// See [`RUNNING`].
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`RUNNING`].
const FAILED: &str = "FAILED";

/// What the holder settles its task with, which is the state's result and — the state being
/// the last one — the execution's own output.
fn settlement_output() -> Value {
    json!({ "approved": true, "by": "the holder of the token" })
}

/// What a row's execution must end as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ends {
    /// Every idle span stayed inside the window the state declared, so the settlement the
    /// holder sent decides and the execution succeeds carrying its document.
    Settled,
    /// A window nothing reported progress inside really did lapse, so the task is closed by
    /// it.
    TimedOut,
}

/// One row: the window the state declares, how often the holder reports progress inside it,
/// and how long its work runs.
struct Row {
    case: &'static str,
    /// `HeartbeatSeconds`, which the language states as a positive integer of seconds.
    window_seconds: u64,
    /// The span between two reports of progress: a fraction of the window, so the row is
    /// decided by the rule and not by how fast the host gets through a turn. `None` for the
    /// holder that reports no progress at all, which does no work either.
    report_every: Option<Duration>,
    /// How long the holder's work runs before it settles the task — more than one window
    /// for a row that reports progress, which is what makes the reset the only thing that
    /// can keep the task open.
    works: Duration,
    ends: Ends,
}

const ROWS: &[Row] = &[
    Row {
        case: "progress reported five times a window, over work outlasting two windows",
        window_seconds: 5,
        report_every: Some(Duration::from_secs(1)),
        works: Duration::from_secs(12),
        ends: Ends::Settled,
    },
    Row {
        case: "progress reported four times a window, over work outlasting two windows",
        window_seconds: 4,
        report_every: Some(Duration::from_secs(1)),
        works: Duration::from_secs(10),
        ends: Ends::Settled,
    },
    // The other side of the same rule: a window nothing reported progress inside closes the
    // task, which is what the member is for.
    Row {
        case: "a holder that never reports progress at all",
        window_seconds: 2,
        report_every: None,
        works: Duration::ZERO,
        ends: Ends::TimedOut,
    },
];

/// The one-state definition a row runs: a callback `Task` that hands its token to
/// `queue_url`, under the window the row declares.
///
/// "Task tokens are generated by Step Functions […] in the context object when a workflow
/// enters a task state", so the token the state minted is readable out of the queue its work
/// was handed to — which is how this suite comes to hold it.
fn definition(queue_url: &str, window_seconds: u64) -> Value {
    json!({
        "StartAt": WAITING_STATE,
        "States": {
            WAITING_STATE: {
                "Type": "Task",
                "Resource": "arn:aws:states:::sqs:sendMessage.waitForTaskToken",
                "Parameters": {
                    "QueueUrl": queue_url,
                    "MessageBody": { "token.$": "$$.Task.Token" },
                },
                "HeartbeatSeconds": window_seconds,
                "End": true,
            },
        },
    })
}

/// The token the state's own call handed to the queue at `queue_url`.
async fn handed_over_token(emulator: &Emulator, case: &str, queue_url: &str) -> String {
    let bodies = emulator.bodies(queue_url, 1).await;
    assert_eq!(
        bodies.len(),
        1,
        "case {case:?}: expected the task's token handed to the queue, got {bodies:?}"
    );
    let carried: Value = serde_json::from_str(&bodies[0])
        .unwrap_or_else(|why| panic!("case {case:?}: the body is JSON: {why}"));
    string(&carried, "token")
}

/// Every row: a report of progress starts the window again, so work that keeps reporting it
/// settles its task however many windows' worth of work it does, while a window nothing
/// reported progress inside closes the task with `States.Timeout`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_report_of_progress_starts_the_heartbeat_window_again() {
    let emulator = Emulator::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let queue_url = emulator.queue(&format!("{QUEUE_PREFIX}-{index}")).await;
        let execution = emulator
            .started(
                &format!("heartbeat-holder-{index}"),
                &definition(&queue_url, row.window_seconds),
            )
            .await;
        let token = handed_over_token(&emulator, case, &queue_url).await;

        // The work the token was handed over for: each turn reports progress on it and
        // looks at the execution, until the work has run for longer than the row asks.
        // A report this service ANSWERS is a task that is still open — a closed one refuses
        // every call quoting its token — so the turns are the rule's own observations.
        let began = Instant::now();
        let mut reported = 0_usize;
        if let Some(every) = row.report_every {
            while began.elapsed() < row.works {
                let (status, answered) = emulator
                    .sfn_answered("SendTaskHeartbeat", &json!({ "taskToken": token }))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: report {reported} of progress on an open task must be \
                     served, got {status} with {answered}"
                );
                reported += 1;
                let described = emulator
                    .sfn("DescribeExecution", &json!({ "executionArn": execution }))
                    .await;
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some(RUNNING),
                    "case {case:?}: the execution waits at a task whose window progress keeps \
                     starting again, got {described}"
                );
                tokio::time::sleep(every).await;
            }
        }

        match row.ends {
            Ends::Settled => {
                // The work the reports were made for is done, so the holder settles the
                // task: its document is what the state — and so the execution — ends with.
                assert!(
                    reported > 1,
                    "case {case:?}: the work must have reported progress more than once, got \
                     {reported} report(s)"
                );
                let (status, answered) = emulator
                    .sfn_answered(
                        "SendTaskSuccess",
                        &json!({
                            "taskToken": token,
                            "output": settlement_output().to_string(),
                        }),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the holder must be able to settle a task it kept alive, \
                     got {status} with {answered}"
                );
                let settled = emulator.settled(&execution, case).await;
                assert_eq!(
                    settled.get("status").and_then(Value::as_str),
                    Some(SUCCEEDED),
                    "case {case:?}: expected {SUCCEEDED} for a task reported alive inside its \
                     {}s window, got {settled}",
                    row.window_seconds
                );
                let carried: Value = serde_json::from_str(&string(&settled, "output"))
                    .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"));
                assert_eq!(
                    carried,
                    settlement_output(),
                    "case {case:?}: expected the document the holder settled with, got {carried}"
                );
                let events = emulator.history(&execution).await;
                assert_eq!(
                    count_of(&events, EVENT_TASK_TIMED_OUT),
                    0,
                    "case {case:?}: no {EVENT_TASK_TIMED_OUT} may be recorded for a task whose \
                     window was started again in time, got {events:?}"
                );
            }
            Ends::TimedOut => {
                let settled = emulator.settled(&execution, case).await;
                assert_eq!(
                    (
                        settled.get("status").and_then(Value::as_str),
                        settled.get("error").and_then(Value::as_str),
                    ),
                    (Some(FAILED), Some(TIMEOUT_ERROR)),
                    "case {case:?}: expected {FAILED} with {TIMEOUT_ERROR} once the window \
                     nothing reported progress inside ran out, got {settled}"
                );
                let events = emulator.history(&execution).await;
                assert_eq!(
                    count_of(&events, EVENT_TASK_TIMED_OUT),
                    1,
                    "case {case:?}: expected one {EVENT_TASK_TIMED_OUT} event for the closed \
                     task, got {events:?}"
                );
            }
        }
    }
}
