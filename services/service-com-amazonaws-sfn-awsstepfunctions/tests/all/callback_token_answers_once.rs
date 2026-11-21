// SPDX-License-Identifier: CC0-1.0

//! How many times ONE callback (`.waitForTaskToken`) task token answers its task, driven
//! over both services' own wire protocols through the production `MultiServiceRouter` with
//! Step Functions and Amazon SQS mounted on ONE engine — so the integration call that hands
//! the token out is really made and the token under test is read back out of the queue it
//! was handed to.
//!
//! RULE: a task token answers its task EXACTLY ONCE. "The token that represents this task"
//! (`SendTaskSuccess$taskToken`) resumes the execution waiting at the state that minted it,
//! and that closes the task — so any later call quoting the same token is refused with
//! `TaskTimedOut`, "the task token has either expired or the task associated with the token
//! has already been closed", and the execution is left exactly as the first answer left it.
//!
//! The rows differ along what the rule generalises over: WHICH call answers first (a success
//! and a failure, the two outcomes a holder of a token may report) and which call quotes the
//! token afterwards (a success, a failure, and a report of progress) — because the token
//! names a closed task whatever the second caller asks for.
//!
//! The harness is `tests/all/callback_timeout_error_handling.rs`'s `Emulator`, which mounts
//! the same two services; `tests/all/main.rs` is this binary's crate root, so it is reached
//! through its module.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{EVENT_TASK_FAILED, EVENT_TASK_SUCCEEDED};

use crate::callback_timeout_error_handling::{count_of, Emulator};
use crate::state_machine_lifecycle::{error_code, string};

/// The state whose callback task every row's token belongs to.
const WAITING_STATE: &str = "WaitForTheHolder";

/// The callback integration that state uses: the task hands its token to a queue and waits
/// until that token is answered.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::sqs:sendMessage.waitForTaskToken";

/// The queue each row's task hands its token to, named after the row: one per row, so one
/// row's token is never read as another's.
const QUEUE_PREFIX: &str = "answered-once";

/// The whole-task bound each row declares, in seconds: far longer than answering a token
/// from this process takes, so nothing but the answer closes the task.
const BOUND_SECONDS: u64 = 120;

/// The modelled error a `SendTask*` quoting a token whose task has been closed is refused
/// with, and the status this service's front door answers a client error with
/// (`smithy_query(query="operation",operation="SendTaskSuccess")`: `TaskTimedOut`,
/// HTTP 400).
const TASK_TIMED_OUT: &str = "TaskTimedOut";
/// See [`TASK_TIMED_OUT`].
const CLIENT_ERROR: u16 = 400;

/// What the error a holder reports a FAILURE with names, and the cause beside it.
const REPORTED_ERROR: &str = "LedgerRejected";
/// See [`REPORTED_ERROR`].
const REPORTED_CAUSE: &str = "the ledger rejected the entry";

/// What a holder reporting SUCCESS settles the task with.
fn settlement() -> Value {
    json!({ "approved": true, "by": "the holder of the token" })
}

/// One call quoting a task token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reply {
    /// `SendTaskSuccess`, which resumes the execution with the document it carries.
    Success,
    /// `SendTaskFailure`, which fails the state with the error it names.
    Failure,
    /// `SendTaskHeartbeat`, which reports the work holding the token is still going.
    Heartbeat,
}

impl Reply {
    /// The operation this call names.
    fn operation(self) -> &'static str {
        match self {
            Self::Success => "SendTaskSuccess",
            Self::Failure => "SendTaskFailure",
            Self::Heartbeat => "SendTaskHeartbeat",
        }
    }

    /// Its request, quoting `token`.
    fn request(self, token: &str) -> Value {
        match self {
            Self::Success => {
                json!({ "taskToken": token, "output": settlement().to_string() })
            }
            Self::Failure => json!({
                "taskToken": token,
                "error": REPORTED_ERROR,
                "cause": REPORTED_CAUSE,
            }),
            Self::Heartbeat => json!({ "taskToken": token }),
        }
    }
}

/// One row: the call that answers the token first, and the call that quotes it afterwards.
struct Row {
    case: &'static str,
    first: Reply,
    again: Reply,
}

const ROWS: &[Row] = &[
    Row {
        case: "a success, quoted again by a success",
        first: Reply::Success,
        again: Reply::Success,
    },
    // The same first answer, quoted again by the OTHER outcome a holder may report: the
    // token names a closed task whatever the second caller asks for.
    Row {
        case: "a success, quoted again by a failure",
        first: Reply::Success,
        again: Reply::Failure,
    },
    // And by a report of progress, which is no outcome at all.
    Row {
        case: "a success, quoted again by a report of progress",
        first: Reply::Success,
        again: Reply::Heartbeat,
    },
    // The other first answer: a failure closes the task just as a success does.
    Row {
        case: "a failure, quoted again by a success",
        first: Reply::Failure,
        again: Reply::Success,
    },
];

/// The definition a row runs: one callback `Task` whose work hands its token to
/// `queue_url`.
///
/// "Task tokens are generated by Step Functions […] in the context object when a workflow
/// enters a task state", so the token is readable out of the queue the work was handed to —
/// which is how this suite comes to hold it.
fn definition(queue_url: &str) -> Value {
    json!({
        "StartAt": WAITING_STATE,
        "States": {
            WAITING_STATE: {
                "Type": "Task",
                "Resource": CALLBACK_RESOURCE,
                "Parameters": {
                    "QueueUrl": queue_url,
                    "MessageBody": { "token.$": "$$.Task.Token" },
                },
                "TimeoutSeconds": BOUND_SECONDS,
                "End": true,
            },
        },
    })
}

/// The token the state handed to the queue at `queue_url`.
async fn handed_over(emulator: &Emulator, case: &str, queue_url: &str) -> String {
    let bodies = emulator.bodies(queue_url, 1).await;
    assert_eq!(
        bodies.len(),
        1,
        "case {case:?}: expected the task's token handed to its queue, got {bodies:?}"
    );
    let carried: Value = serde_json::from_str(&bodies[0])
        .unwrap_or_else(|why| panic!("case {case:?}: the body is JSON: {why}"));
    string(&carried, "token")
}

/// Every row: the first call answers the task and closes it, and the next call quoting the
/// same token is refused without changing anything the first one settled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_task_token_answers_its_task_exactly_once() {
    let emulator = Emulator::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let queue_url = emulator.queue(&format!("{QUEUE_PREFIX}-{index}")).await;
        let execution = emulator
            .started(&format!("answered-once-{index}"), &definition(&queue_url))
            .await;
        let token = handed_over(&emulator, case, &queue_url).await;

        // The first call answers the task, so it is served and the execution leaves
        // RUNNING carrying what that call reported.
        let (status, answered) = emulator
            .sfn_answered(row.first.operation(), &row.first.request(&token))
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: expected the first {} to be served, got {status} with {answered}",
            row.first.operation()
        );
        let settled = emulator.settled(&execution, case).await;
        let expected = match row.first {
            Reply::Success => (
                Some("SUCCEEDED"),
                Some(settlement().to_string()),
                None,
                None,
            ),
            Reply::Failure => (
                Some("FAILED"),
                None,
                Some(REPORTED_ERROR),
                Some(REPORTED_CAUSE),
            ),
            Reply::Heartbeat => unreachable!("a report of progress answers no task"),
        };
        assert_eq!(
            (
                settled.get("status").and_then(Value::as_str),
                settled.get("output").and_then(Value::as_str).map(str::to_string),
                settled.get("error").and_then(Value::as_str),
                settled.get("cause").and_then(Value::as_str),
            ),
            expected,
            "case {case:?}: expected the execution to carry what the first answer reported, \
             got {settled}"
        );

        // The next call quoting the SAME token is refused: the token names a task that has
        // already been closed.
        let (status, refused) = emulator
            .sfn_answered(row.again.operation(), &row.again.request(&token))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR, TASK_TIMED_OUT),
            "case {case:?}: expected the duplicate {} to be refused with {TASK_TIMED_OUT}, got \
             {status} with {refused}",
            row.again.operation()
        );

        // And it changed nothing: the execution still reports what the first answer settled
        // it with, and the task was answered once in its history.
        let after = emulator
            .sfn("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        assert_eq!(
            after.get("status"),
            settled.get("status"),
            "case {case:?}: expected the refused duplicate to leave the execution as it was, \
             got {after}"
        );
        assert_eq!(
            after.get("output"),
            settled.get("output"),
            "case {case:?}: expected the refused duplicate to leave the output as it was, got \
             {after}"
        );
        let events = emulator.history(&execution).await;
        let answers = count_of(&events, EVENT_TASK_SUCCEEDED) + count_of(&events, EVENT_TASK_FAILED);
        assert_eq!(
            answers, 1,
            "case {case:?}: expected the task answered exactly once, got {} succeeded and {} \
             failed",
            count_of(&events, EVENT_TASK_SUCCEEDED),
            count_of(&events, EVENT_TASK_FAILED)
        );
    }
}
