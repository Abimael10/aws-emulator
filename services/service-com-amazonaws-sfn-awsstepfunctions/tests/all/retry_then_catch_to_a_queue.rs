// SPDX-License-Identifier: CC0-1.0

//! What a `Task` state whose work KEEPS FAILING does next: its `Retry` schedule, then its
//! `Catch` transition, then the work of the state that transition names — driven over three
//! services' own wire protocols through the production `MultiServiceRouter` with Step
//! Functions, Lambda and Amazon SQS mounted on ONE engine, so the work that fails really
//! runs and the message the catch branch sends is read back out of the queue through Amazon
//! SQS's own API.
//!
//! RULE: a `Task` state answers for its work's failure with the handlers it declares —
//! "Retry […] an array of objects, called retriers" is tried first, and once a retrier's
//! `MaxAttempts` is spent the failure is handed to the `Catch` whose catcher matches it,
//! which "transitions to the state specified in the Next field" with the error output as
//! that state's input — so the catch branch's own `Task` really performs its work and the
//! execution reaches a terminal status by that branch. A member such a task's integration
//! documents as a DOCUMENT while the invoked operation types it as a string travels as that
//! document's JSON text, so a structured payload is what the far end receives.
//!
//! The rows differ along what the rule generalises over: the KIND of work that keeps failing
//! (a Lambda function that raises, and a send to a queue the account does not hold), the
//! retry schedule the state declares and so how many attempts the failure is handed over for
//! (none, one retry, two retries with backoff), and the JSON TYPE the catch branch's own
//! `MessageBody` is written as (a document, a string, a list, a number).
//!
//! The harness (`Chain`, `given_function_of`, `invoked`, `ran`, `sqs`) is
//! `tests/all/lambda_task_control_flow.rs`'s, and `event_types` is
//! `tests/all/lambda_function_timeout.rs`'s; `tests/all/main.rs` is this binary's crate
//! root, so each is reached through its module.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{EVENT_TASK_FAILED, EVENT_TASK_SUCCEEDED};

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;

/// The state whose work keeps failing, and the state its `Catch` transitions to.
const FAILING_STATE: &str = "DoTheWork";
/// See [`FAILING_STATE`].
const CATCH_STATE: &str = "ReportTheFailure";

/// Where the catcher places the error output, which the catch branch's own parameters read
/// the failure out of.
const ERROR_PATH: &str = "$.caught";

/// The wildcard the Amazon States Language documents as matching any task failure, which
/// every row's retrier and catcher name.
const STATES_TASK_FAILED: &str = "States.TaskFailed";

/// The handler the Lambda rows invoke: it always raises, so the work that state's task does
/// fails on every attempt.
const RAISING_HANDLER: &str = r#"
class LedgerUnavailable(Exception):
    pass


def handler(event, context):
    raise LedgerUnavailable("the ledger could not be reached")
"#;

/// The exception class [`RAISING_HANDLER`] raises, which is the `errorType` of the error
/// document the run produces and so the name the failed task reports.
const RAISED_ERROR: &str = "LedgerUnavailable";

/// The name a refused send to a queue the account does not hold is reported under: Amazon
/// SQS's modelled `QueueDoesNotExist`, as an SDK's class for it
/// (`smithy_query(query="operation",operation="SendMessage",model_path="services/
/// service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`).
const QUEUE_GONE_ERROR: &str = "SQS.QueueDoesNotExistException";

/// The function's own `Timeout`, in seconds: well past what raising takes, so no row is
/// decided by that bound.
const FUNCTION_TIMEOUT_SECONDS: u64 = 120;

/// A note the document-valued body carries beside the error, so what arrives is a document
/// with more than one member rather than a value that happens to match.
const NOTE: &str = "routed by the catch branch";

/// The queue each row's catch branch sends to, named after the row: one per row, so one
/// row's message is never read as another's.
const QUEUE_PREFIX: &str = "caught-failures";

/// How long a row waits for the catch branch's message to arrive, and how long one look at
/// the queue waits before looking again. Generous next to the retry intervals the rows
/// declare and next to the invocations their work is made of, so a loaded host cannot fail a
/// row the emulator is still working through. A ceiling on patience, never a span anything
/// here counts on.
const PATIENCE: Duration = Duration::from_secs(120);
/// See [`PATIENCE`].
const POLL: Duration = Duration::from_millis(25);

/// What the work the failing state does IS.
#[derive(Clone, Copy, Debug)]
enum Work {
    /// A Lambda invocation whose function raises, so the task fails under the error
    /// document the run produced.
    RaisingFunction,
    /// A send to a queue the account does not hold, so the task fails under the error the
    /// invoked service answered.
    SendToAGoneQueue,
}

impl Work {
    /// The error name the task reports for this work.
    fn error(self) -> &'static str {
        match self {
            Self::RaisingFunction => RAISED_ERROR,
            Self::SendToAGoneQueue => QUEUE_GONE_ERROR,
        }
    }
}

/// How the catch branch's `MessageBody` is written, which is what the queue must receive.
///
/// The rule is one: a member the `sqs:sendMessage` integration documents as a document
/// while `SendMessage$MessageBody` types it as a String travels as the JSON TEXT of the
/// document the state built, and a value already a string travels unchanged.
#[derive(Clone, Copy, Debug)]
enum Body {
    /// A DOCUMENT whose members the error output fills.
    Document,
    /// The error NAME alone: a string the operation already types.
    Text,
    /// A LIST carrying the error name.
    List,
    /// A NUMBER, which carries nothing of the failure at all.
    Number,
}

/// The number the [`Body::Number`] row's body is written as.
const COUNTED: u64 = 3;

impl Body {
    /// The `Parameters` the catch branch's `Task` state declares for a send to `queue_url`.
    fn parameters(self, queue_url: &str) -> Value {
        match self {
            Self::Document => json!({
                "QueueUrl": queue_url,
                "MessageBody": {
                    "caught.$": format!("{ERROR_PATH}.Error"),
                    "note": NOTE,
                },
            }),
            Self::Text => json!({
                "QueueUrl": queue_url,
                "MessageBody.$": format!("{ERROR_PATH}.Error"),
            }),
            Self::List => json!({
                "QueueUrl": queue_url,
                "MessageBody": [{ "caught.$": format!("{ERROR_PATH}.Error") }, NOTE],
            }),
            Self::Number => json!({ "QueueUrl": queue_url, "MessageBody": COUNTED }),
        }
    }

    /// The document the queue must receive once the work that failed reported `error`, and
    /// `None` for the body written as text — which arrives as that text and is no JSON
    /// document at all.
    fn delivered(self, error: &str) -> Option<Value> {
        match self {
            Self::Document => Some(json!({ "caught": error, "note": NOTE })),
            Self::Text => None,
            Self::List => Some(json!([{ "caught": error }, NOTE])),
            Self::Number => Some(json!(COUNTED)),
        }
    }
}

/// One row: the work that keeps failing, the retry schedule in force on it, the attempts
/// that schedule asks for, and how the catch branch's own body is written.
struct Row {
    case: &'static str,
    work: Work,
    /// The retrier's `MaxAttempts`, and `None` for a state declaring no `Retry` at all.
    retries: Option<u64>,
    /// The `BackoffRate` that retrier declares.
    backoff: f64,
    /// How many times the work is handed over: once per attempt the schedule allows.
    attempts: usize,
    body: Body,
}

const ROWS: &[Row] = &[
    Row {
        case: "a function that keeps raising, retried twice with backoff, caught into a \
               document",
        work: Work::RaisingFunction,
        retries: Some(2),
        backoff: 2.0,
        attempts: 3,
        body: Body::Document,
    },
    // The same work with no retrier at all: the catcher answers the first failure.
    Row {
        case: "the same work with no Retry at all, caught into the error's own name",
        work: Work::RaisingFunction,
        retries: None,
        backoff: 1.0,
        attempts: 1,
        body: Body::Text,
    },
    // Another KIND of failing work — a call the invoked service refuses — retried once.
    Row {
        case: "a send to a queue the account does not hold, retried once, caught into a list",
        work: Work::SendToAGoneQueue,
        retries: Some(1),
        backoff: 1.0,
        attempts: 2,
        body: Body::List,
    },
    // And the same refused call under a flat schedule of two retries, carrying a body that
    // says nothing of the failure.
    Row {
        case: "the same refused call retried twice at a flat rate, caught into a number",
        work: Work::SendToAGoneQueue,
        retries: Some(2),
        backoff: 1.0,
        attempts: 3,
        body: Body::Number,
    },
];

/// The definition one row runs: the failing state with the row's own handlers, and the catch
/// branch's own `Task` sending the row's own body to `queue_url`.
fn definition(row: &Row, function_arn: &str, gone_queue_url: &str, queue_url: &str) -> Value {
    let mut failing = match row.work {
        Work::RaisingFunction => json!({
            "Type": "Task",
            "Resource": "arn:aws:states:::lambda:invoke",
            "Parameters": { "FunctionName": function_arn, "Payload": {} },
            "End": true,
        }),
        Work::SendToAGoneQueue => json!({
            "Type": "Task",
            "Resource": "arn:aws:states:::sqs:sendMessage",
            "Parameters": { "QueueUrl": gone_queue_url, "MessageBody": "never delivered" },
            "End": true,
        }),
    };
    if let Some(max_attempts) = row.retries {
        failing["Retry"] = json!([{
            "ErrorEquals": [STATES_TASK_FAILED],
            "IntervalSeconds": 1,
            "MaxAttempts": max_attempts,
            "BackoffRate": row.backoff,
        }]);
    }
    failing["Catch"] = json!([{
        "ErrorEquals": [STATES_TASK_FAILED],
        "ResultPath": ERROR_PATH,
        "Next": CATCH_STATE,
    }]);
    json!({
        "StartAt": FAILING_STATE,
        "States": {
            FAILING_STATE: failing,
            CATCH_STATE: {
                "Type": "Task",
                "Resource": "arn:aws:states:::sqs:sendMessage",
                "Parameters": row.body.parameters(queue_url),
                "End": true,
            },
        },
    })
}

/// The bodies the queue at `url` holds: looked at again until one has arrived or
/// [`PATIENCE`] runs out, and then once MORE, so a second message the catch branch should
/// not have sent is collected rather than missed. How many really arrived is the caller's
/// own assertion.
async fn delivered(chain: &Chain, case: &str, url: &str) -> Vec<String> {
    let began = Instant::now();
    let mut collected: Vec<String> = Vec::new();
    loop {
        let received = chain
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        if let Some(messages) = received.get("Messages").and_then(Value::as_array) {
            collected.extend(messages.iter().map(|message| string(message, "Body")));
        }
        // The execution is terminal before the queue is read, so nothing further is owed
        // once a message is there: one more look settles whether a second one was sent.
        if !collected.is_empty() {
            let again = chain
                .sqs(
                    case,
                    "ReceiveMessage",
                    &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
                )
                .await;
            if let Some(messages) = again.get("Messages").and_then(Value::as_array) {
                collected.extend(messages.iter().map(|message| string(message, "Body")));
            }
            return collected;
        }
        if began.elapsed() >= PATIENCE {
            return collected;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Every row: the retry schedule is spent, the catcher's transition runs, and the catch
/// branch's own send really delivers what the state built.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spent_retry_schedule_hands_the_failure_to_the_catch_branchs_own_work() {
    let chain = Chain::with_sqs();
    let function = chain
        .given_function_of("raiser", RAISING_HANDLER, Some(FUNCTION_TIMEOUT_SECONDS))
        .await;
    // One invocation BEFORE any row, so the execution environment this function's runs need
    // is standing rather than built inside a row's own retry schedule.
    chain
        .invoked("warming the function", "raiser", &json!({}))
        .await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One queue per row, so one row's message is never counted in another's — and a url
        // addressing a queue nobody created, which is what one kind of failing work
        // addresses.
        let name = format!("{QUEUE_PREFIX}-{index}");
        let created = chain
            .sqs(case, "CreateQueue", &json!({ "QueueName": name }))
            .await;
        let queue_url = string(&created, "QueueUrl");
        let gone_queue_url = queue_url.replace(&name, "no-such-queue");
        let described = chain
            .ran(
                case,
                &format!("retry-then-catch-{index}"),
                &definition(row, &function, &gone_queue_url, &queue_url),
                &json!({}),
            )
            .await;
        // The catch branch finished, so the execution reached its terminal status by that
        // branch rather than by the failure.
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the catch branch to carry the execution to its end, got \
             {described}"
        );

        // The work was handed over once per attempt the row's schedule allows, and the catch
        // branch's own task is the one that succeeded.
        let answered = chain
            .sfn(
                case,
                "GetExecutionHistory",
                &json!({ "executionArn": string(&described, "executionArn") }),
            )
            .await;
        let types = event_types(
            &answered
                .get("events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        assert_eq!(
            types.iter().filter(|kind| *kind == EVENT_TASK_FAILED).count(),
            row.attempts,
            "case {case:?}: expected {} failed attempt(s), got {types:?}",
            row.attempts
        );
        assert_eq!(
            types
                .iter()
                .filter(|kind| *kind == EVENT_TASK_SUCCEEDED)
                .count(),
            1,
            "case {case:?}: expected the catch branch's own work to be the one that \
             succeeded, got {types:?}"
        );

        // And what the far end really received: one message, carrying what the catch
        // branch's `MessageBody` was written as.
        let bodies = delivered(&chain, case, &queue_url).await;
        assert_eq!(
            bodies.len(),
            1,
            "case {case:?}: expected exactly one message from the catch branch, got {bodies:?}"
        );
        let body = &bodies[0];
        let error = row.work.error();
        match row.body.delivered(error) {
            Some(document) => assert_eq!(
                serde_json::from_str::<Value>(body).ok(),
                Some(document.clone()),
                "case {case:?}: expected the JSON text of {document}, got {body:?}"
            ),
            None => assert_eq!(
                body, error,
                "case {case:?}: expected the body written as text to arrive unchanged, got \
                 {body:?}"
            ),
        }
    }
}
