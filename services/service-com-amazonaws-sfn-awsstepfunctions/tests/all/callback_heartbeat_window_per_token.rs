// SPDX-License-Identifier: CC0-1.0

//! WHOSE window a report of progress starts again, on an execution that waits at TWO
//! callback (`.waitForTaskToken`) tasks in turn: Step Functions and Amazon SQS over one
//! engine, each task's token read back out of the queue its own integration call sent it to,
//! and every `SendTaskHeartbeat` made from here.
//!
//! RULE: "this action resets the Heartbeat clock" (`SendTaskHeartbeat`), and
//! "HeartbeatSeconds […] if more time than the specified seconds elapses between heartbeats
//! from the task, this state fails with a States.Timeout error name". The call quotes ONE
//! task token — "the token must be copied and included in subsequent calls to
//! SendTaskHeartbeat" — so what it starts again is the window of the task that token names
//! and of no other: an execution whose first task is reported alive over several windows'
//! worth of work carries on at its next state, where the window that state declared is open
//! from its own hand-over and is reset only by reports quoting ITS token. Nothing the first
//! task's reports did keeps the second one alive.
//!
//! What this adds next to [`crate::callback_heartbeat_window_holder`], which spans the
//! window's length and how often progress is reported inside it, is the number of TOKENS one
//! execution hands out: the two rows differ in which of the execution's two tasks is
//! reported alive, so a window kept open per EXECUTION rather than per task fails here, and
//! so does one that a later task's lapse closes retrospectively.
//!
//! The holder is the test, so every span the rule turns on is one this process measures and
//! spends itself: nothing is unpacked, no interpreter is started, and each report of
//! progress is answered in process. No row's outcome is decided by how fast the host is.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    asl::TIMEOUT_ERROR, EVENT_TASK_SUBMITTED, EVENT_TASK_SUCCEEDED, EVENT_TASK_TIMED_OUT,
};

use crate::callback_timeout_error_handling::{count_of, Emulator};
use crate::state_machine_lifecycle::string;

/// The callback integration both states use: the task hands its token to a queue and waits
/// until that token is answered.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::sqs:sendMessage.waitForTaskToken";

/// The two states a row's execution waits at, in order.
const FIRST_STATE: &str = "WaitForTheFirstHolder";
/// See [`FIRST_STATE`].
const SECOND_STATE: &str = "WaitForTheSecondHolder";

/// The queues the two states hand their tokens to: one per state and per row, so one
/// task's token is never read as another's.
const QUEUE_PREFIX: &str = "heartbeat-window-per-token";

/// The statuses an execution of these machines is read in (`ExecutionStatus`).
const RUNNING: &str = "RUNNING";
/// See [`RUNNING`].
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`RUNNING`].
const FAILED: &str = "FAILED";

/// The window each state declares as `HeartbeatSeconds`, which the language states as a
/// positive integer of seconds.
const WINDOW_SECONDS: u64 = 4;

/// The span between two reports of progress on one token: a quarter of the window, so a row
/// is decided by the rule and not by how fast the host gets through a turn.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// How long the work one token was handed over for runs: more than one window, so the task
/// stays open only because the reports quoting its own token keep starting its window again.
const WORKS: Duration = Duration::from_secs(6);

/// What a holder settles its task with, naming which of the two it is: the second one's
/// document is the last state's result and so the execution's own output.
fn settlement_output(holder: &str) -> Value {
    json!({ "approved": true, "by": holder })
}

/// How the two holders name themselves in the documents they settle with.
const FIRST_HOLDER: &str = "the holder of the first task's token";
/// See [`FIRST_HOLDER`].
const SECOND_HOLDER: &str = "the holder of the second task's token";

/// What a row's execution must end as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ends {
    /// Both tasks were reported alive inside their own windows and settled, so the second
    /// settlement decides and the execution succeeds carrying its document.
    Settled,
    /// The second task's window really did lapse, so that task is closed by it — and the
    /// first task, which was settled, is not reopened by anything.
    TimedOut,
}

/// One row: whether the holder of the SECOND task's token reports progress on it, and what
/// the execution must therefore end as.
struct Row {
    case: &'static str,
    reports_on_the_second: bool,
    ends: Ends,
}

const ROWS: &[Row] = &[
    Row {
        case: "both tasks reported alive on their own tokens, over work outlasting a window",
        reports_on_the_second: true,
        ends: Ends::Settled,
    },
    // The other side of the same rule: the first task was reported alive many times over
    // and settled, and the window the SECOND task declared is still closed by the reports
    // nothing made on its own token.
    Row {
        case: "the first task reported alive and settled, the second reported not at all",
        reports_on_the_second: false,
        ends: Ends::TimedOut,
    },
];

/// The two-state definition a row runs: a callback `Task` that hands its token to
/// `first_queue` and carries on at a second one that hands its own token to `second_queue`,
/// each under a window of its own.
///
/// "Task tokens are generated by Step Functions […] in the context object when a workflow
/// enters a task state", so each state's token is readable out of the queue its work was
/// handed to — which is how this suite comes to hold both of them.
fn definition(first_queue: &str, second_queue: &str) -> Value {
    json!({
        "StartAt": FIRST_STATE,
        "States": {
            FIRST_STATE: {
                "Type": "Task",
                "Resource": CALLBACK_RESOURCE,
                "Parameters": {
                    "QueueUrl": first_queue,
                    "MessageBody": { "token.$": "$$.Task.Token" },
                },
                "HeartbeatSeconds": WINDOW_SECONDS,
                "Next": SECOND_STATE,
            },
            SECOND_STATE: {
                "Type": "Task",
                "Resource": CALLBACK_RESOURCE,
                "Parameters": {
                    "QueueUrl": second_queue,
                    "MessageBody": { "token.$": "$$.Task.Token" },
                },
                "HeartbeatSeconds": WINDOW_SECONDS,
                "End": true,
            },
        },
    })
}

/// The token the state whose work was handed to `queue_url` minted for it.
async fn handed_over_token(emulator: &Emulator, case: &str, which: &str, queue_url: &str) -> String {
    let bodies = emulator.bodies(queue_url, 1).await;
    assert_eq!(
        bodies.len(),
        1,
        "case {case:?}: expected {which} task's token handed to its queue, got {bodies:?}"
    );
    let carried: Value = serde_json::from_str(&bodies[0])
        .unwrap_or_else(|why| panic!("case {case:?}: the body is JSON: {why}"));
    string(&carried, "token")
}

/// Report progress on `token` every [`REPORT_EVERY`] until the work has run for [`WORKS`],
/// and answer how many reports this service served.
///
/// Each report is asserted SERVED and the execution asserted still waiting: a closed task
/// refuses every call quoting its token, so the turns are the rule's own observations.
async fn reported_alive(emulator: &Emulator, case: &str, execution: &str, token: &str) -> usize {
    let began = Instant::now();
    let mut reported = 0_usize;
    while began.elapsed() < WORKS {
        let (status, answered) = emulator
            .sfn_answered("SendTaskHeartbeat", &json!({ "taskToken": token }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: report {reported} of progress on an open task must be served, got \
             {status} with {answered}"
        );
        reported += 1;
        let described = emulator
            .sfn("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(RUNNING),
            "case {case:?}: the execution waits at a task whose window its own reports keep \
             starting again, got {described}"
        );
        tokio::time::sleep(REPORT_EVERY).await;
    }
    assert!(
        reported > 1,
        "case {case:?}: the work must have reported progress more than once, got {reported} \
         report(s)"
    );
    reported
}

/// Settle the task `token` names with the document `holder` settles it with.
async fn settled_by(emulator: &Emulator, case: &str, holder: &str, token: &str) {
    let (status, answered) = emulator
        .sfn_answered(
            "SendTaskSuccess",
            &json!({
                "taskToken": token,
                "output": settlement_output(holder).to_string(),
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {holder} must be able to settle the task it kept alive, got {status} \
         with {answered}"
    );
}

/// Both rows: a report of progress starts the window of the task ITS TOKEN names again, so
/// an execution whose every task is reported alive settles however long its work runs, while
/// the window a later task declared is closed by the reports nothing made on that task's own
/// token — whatever was reported on the task before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_report_of_progress_starts_the_window_of_its_own_task_again() {
    let emulator = Emulator::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let first_queue = emulator.queue(&format!("{QUEUE_PREFIX}-{index}-first")).await;
        let second_queue = emulator
            .queue(&format!("{QUEUE_PREFIX}-{index}-second"))
            .await;
        let execution = emulator
            .started(
                &format!("heartbeat-per-token-{index}"),
                &definition(&first_queue, &second_queue),
            )
            .await;

        // The FIRST task: reported alive on its own token over work outlasting its window,
        // and then settled, which is what carries the execution on to the second state.
        let first_token = handed_over_token(&emulator, case, "the first", &first_queue).await;
        reported_alive(&emulator, case, &execution, &first_token).await;
        settled_by(&emulator, case, FIRST_HOLDER, &first_token).await;

        // The SECOND task, whose window is open from its OWN hand-over: the token it minted
        // is a second one, and only reports quoting it start that window again.
        let second_token = handed_over_token(&emulator, case, "the second", &second_queue).await;
        assert_ne!(
            second_token, first_token,
            "case {case:?}: each task state mints a token of its own, got the same one twice"
        );
        if row.reports_on_the_second {
            reported_alive(&emulator, case, &execution, &second_token).await;
            settled_by(&emulator, case, SECOND_HOLDER, &second_token).await;
        }

        let settled = emulator.settled(&execution, case).await;
        let events = emulator.history(&execution).await;
        // Both tasks really were handed over, whatever each of them ended as: a row that
        // never reached its second hand-over would prove nothing about a second window.
        assert_eq!(
            count_of(&events, EVENT_TASK_SUBMITTED),
            2,
            "case {case:?}: expected both tasks handed over, got {events:?}"
        );
        match row.ends {
            Ends::Settled => {
                assert_eq!(
                    settled.get("status").and_then(Value::as_str),
                    Some(SUCCEEDED),
                    "case {case:?}: expected {SUCCEEDED} for two tasks each reported alive \
                     inside its own {WINDOW_SECONDS}s window, got {settled}"
                );
                let carried: Value = serde_json::from_str(&string(&settled, "output"))
                    .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"));
                assert_eq!(
                    carried,
                    settlement_output(SECOND_HOLDER),
                    "case {case:?}: expected the document the last holder settled with, got \
                     {carried}"
                );
                assert_eq!(
                    count_of(&events, EVENT_TASK_TIMED_OUT),
                    0,
                    "case {case:?}: no {EVENT_TASK_TIMED_OUT} may be recorded for tasks whose \
                     windows were started again in time, got {events:?}"
                );
                assert_eq!(
                    count_of(&events, EVENT_TASK_SUCCEEDED),
                    2,
                    "case {case:?}: expected both settlements recorded, got {events:?}"
                );
            }
            Ends::TimedOut => {
                assert_eq!(
                    (
                        settled.get("status").and_then(Value::as_str),
                        settled.get("error").and_then(Value::as_str),
                    ),
                    (Some(FAILED), Some(TIMEOUT_ERROR)),
                    "case {case:?}: expected {FAILED} with {TIMEOUT_ERROR} once the second \
                     task's window ran out, got {settled}"
                );
                // Exactly the second task was closed by its window, and exactly the first one
                // was settled: the reports made on the first token neither kept the second
                // task alive nor were undone by its lapse.
                assert_eq!(
                    count_of(&events, EVENT_TASK_TIMED_OUT),
                    1,
                    "case {case:?}: expected one {EVENT_TASK_TIMED_OUT} event, for the task \
                     nothing reported progress on, got {events:?}"
                );
                assert_eq!(
                    count_of(&events, EVENT_TASK_SUCCEEDED),
                    1,
                    "case {case:?}: expected the settled task's own success recorded once, got \
                     {events:?}"
                );
            }
        }
    }
}
