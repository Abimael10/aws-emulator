// SPDX-License-Identifier: CC0-1.0

//! What a callback (`.waitForTaskToken`) task's own BOUND does to the state that
//! declared it, driven over both services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions and Amazon SQS mounted on ONE engine — the
//! shape `core/router/src/lib.rs` mounts them in — so the integration call that hands the
//! token out is really made and the tokens it carried are read back out of the queue it
//! sent them to.
//!
//! RULE: a bound a `Task` state declares running out raises the Amazon States Language's
//! own `States.Timeout` AT THAT STATE — "if the task runs longer than the specified
//! seconds, this state fails with a States.Timeout error name" — so the state answers for
//! it exactly as it answers for any other failure of its work: a `Retry` whose retrier
//! names the error makes the state's call again, a `Catch` whose catcher names it carries
//! the execution on at that catcher's `Next` with the error output as the state's output,
//! and a state that answers it in neither way fails the execution with it.
//!
//! The table spans what the rule generalises over: WHICH handler answers (a catcher
//! naming the error, the `States.ALL` wildcard, a retrier and then a catcher), which
//! handlers do NOT answer it (a catcher naming `States.TaskFailed`, the wildcard the
//! language documents as matching "any known error name except States.Timeout", and one
//! naming another service's error), which of the state's two bounds ran out
//! (`TimeoutSeconds` and `HeartbeatSeconds`, for which the language states the same error
//! name), and how many attempts the state's work is therefore handed over for — counted
//! both in the history and in the queue the work was handed to.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    asl::TIMEOUT_ERROR, EVENT_TASK_SUBMITTED, EVENT_TASK_TIMED_OUT,
};
use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::state_machine_lifecycle::{error_code, string, REGION, ROLE_ARN};

/// The state whose callback task every row below waits at.
const WAITING_STATE: &str = "WaitForCallback";

/// The state a catcher that answers for the timeout carries the execution on at.
const CAUGHT_STATE: &str = "Caught";

/// The queue each row's callback integration hands its token to: one per row, named
/// after it.
const QUEUE_PREFIX: &str = "callback-timeouts";

/// The bound each row declares, in seconds: the smallest a definition may state, so a
/// row waits for the condition it is about and no longer.
const BOUND_SECONDS: u64 = 1;

/// The modelled error a `SendTask*` is refused with for a token whose task has been
/// closed: "the task token has either expired or the task associated with the token has
/// already been closed"
/// (`smithy_query(query="operation",operation="SendTaskSuccess")`).
const TASK_TIMED_OUT: &str = "TaskTimedOut";

/// How long a row waits for the execution to leave `RUNNING`, and for the messages its
/// attempts owe. Generous next to the one-second bound, so a loaded host cannot fail a
/// row the emulator is still working through.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long one look at the execution or the queue waits before looking again.
const POLL: Duration = Duration::from_millis(25);

/// Step Functions and Amazon SQS over ONE engine, so the call that hands a token out is
/// served by the same store the test reads the tokens back from.
///
/// `pub(crate)` because a sibling suite whose rule is about the same hand-over reaches it
/// through this harness rather than opening a second seam onto the two wire protocols
/// ([`crate::callback_heartbeat_window_holder`]).
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One `awsJson1_0` RPC call to the service whose signing name is `signing_name`, as
    /// its status and parsed body.
    async fn answered(
        &self,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One `awsJson1_0` RPC call to the service whose signing name is `signing_name`.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let (status, parsed) = self
            .answered(signing_name, target_prefix, operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation}: expected 200, got {status} with body {parsed}"
        );
        parsed
    }

    /// The status and the error class one Step Functions call was REFUSED with.
    async fn sfn_refusal(&self, operation: &str, body: &Value) -> (u16, String) {
        let (status, parsed) = self.sfn_answered(operation, body).await;
        (status, error_code(&parsed))
    }

    /// The status and the parsed body one Step Functions call was answered with, whatever
    /// that status is: for a caller that asserts on the status itself.
    pub(crate) async fn sfn_answered(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.answered("states", "AWSStepFunctions", operation, body)
            .await
    }

    pub(crate) async fn sfn(&self, operation: &str, body: &Value) -> Value {
        self.rpc("states", "AWSStepFunctions", operation, body).await
    }

    async fn sqs(&self, operation: &str, body: &Value) -> Value {
        self.rpc("sqs", "AmazonSQS", operation, body).await
    }

    /// The url of a queue a row's callback tasks hand their tokens to. One per row, so
    /// one row's attempts are never counted in another's.
    pub(crate) async fn queue(&self, name: &str) -> String {
        let created = self
            .sqs("CreateQueue", &json!({ "QueueName": name }))
            .await;
        string(&created, "QueueUrl")
    }

    /// Create a state machine and start one execution of it, reporting its ARN.
    pub(crate) async fn started(&self, name: &str, definition: &Value) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        let machine_arn = string(&created, "stateMachineArn");
        let started = self
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine_arn, "name": "only" }),
            )
            .await;
        string(&started, "executionArn")
    }

    /// The execution `execution_arn`, once it has left `RUNNING`.
    pub(crate) async fn settled(&self, execution_arn: &str, case: &str) -> Value {
        let began = Instant::now();
        loop {
            let described = self
                .sfn(
                    "DescribeExecution",
                    &json!({ "executionArn": execution_arn }),
                )
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, got {described:?}"
            );
            ::tokio::time::sleep(POLL).await;
        }
    }

    /// The event types the history of `execution_arn` carries, with the details of each.
    pub(crate) async fn history(&self, execution_arn: &str) -> Vec<Value> {
        let answered = self
            .sfn(
                "GetExecutionHistory",
                &json!({ "executionArn": execution_arn, "maxResults": 200 }),
            )
            .await;
        answered
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The bodies the queue at `url` holds, looked at again until `wanted` of them have
    /// arrived or `PATIENCE` runs out. How many really arrived is the caller's own
    /// assertion.
    pub(crate) async fn bodies(&self, url: &str, wanted: usize) -> Vec<String> {
        let began = Instant::now();
        let mut collected: Vec<String> = Vec::new();
        loop {
            let received = self
                .sqs(
                    "ReceiveMessage",
                    &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
                )
                .await;
            if let Some(messages) = received.get("Messages").and_then(Value::as_array) {
                collected.extend(messages.iter().map(|message| string(message, "Body")));
            }
            if collected.len() >= wanted || began.elapsed() >= PATIENCE {
                return collected;
            }
            ::tokio::time::sleep(POLL).await;
        }
    }
}

/// A definition whose one callback `Task` waits for a token nothing will answer, carrying
/// `bounds` (the bound that runs out) and `handlers` (its `Retry` and `Catch`).
///
/// The work is a send to `queue_url` whose body carries `$$.Task.Token` — "task tokens
/// are generated by Step Functions [...] in the context object when a workflow enters a
/// task state" — so each attempt's token is readable out of the queue the work was handed
/// to.
fn definition(queue_url: &str, bounds: &Value, handlers: &Value) -> Value {
    let mut state = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::sqs:sendMessage.waitForTaskToken",
        "Parameters": {
            "QueueUrl": queue_url,
            "MessageBody": { "token.$": "$$.Task.Token" },
        },
        "End": true,
    });
    let members = state.as_object_mut().expect("the state is an object");
    for extra in [bounds, handlers] {
        if let Some(extra) = extra.as_object() {
            for (member, value) in extra {
                members.insert(member.clone(), value.clone());
            }
        }
    }
    let mut states = json!({ WAITING_STATE: state });
    // The catcher's own branch, which exists only where a catcher names it: a state no
    // transition reaches is not a definition this service accepts.
    if handlers.get("Catch").is_some() {
        states
            .as_object_mut()
            .expect("the states are an object")
            .insert(
                CAUGHT_STATE.to_string(),
                // It passes the error output on, so what the execution ends with is what
                // the catcher was handed.
                json!({ "Type": "Pass", "End": true }),
            );
    }
    json!({ "StartAt": WAITING_STATE, "States": states })
}

/// A `Catch` whose one catcher names `errors` and carries the execution on at
/// [`CAUGHT_STATE`].
fn catch(errors: &[&str]) -> Value {
    json!({ "Catch": [{ "ErrorEquals": errors, "Next": CAUGHT_STATE }] })
}

/// How many events of `event_type` the history holds.
pub(crate) fn count_of(events: &[Value], event_type: &str) -> usize {
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .count()
}

/// One row: which bound runs out, what the state declares about the error, and what the
/// execution must therefore do.
struct Row {
    case: &'static str,
    /// The bound the state declares, which is what runs out.
    bounds: Value,
    /// Its `Retry` and `Catch`.
    handlers: Value,
    /// Whether the execution ends SUCCEEDED through the catcher's branch.
    caught: bool,
    /// How many times the state's work is handed over: once, and once more per attempt a
    /// retrier makes.
    attempts: usize,
}

/// RULE: the state's own `Retry` and `Catch` answer for the `States.Timeout` its bound
/// raises, exactly as they answer for any other failure of its work.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_callback_bound_is_answered_by_the_states_own_error_handling() {
    let timeout_bound = json!({ "TimeoutSeconds": BOUND_SECONDS });
    let rows = [
        Row {
            case: "a catcher naming the error",
            bounds: timeout_bound.clone(),
            handlers: catch(&[TIMEOUT_ERROR]),
            caught: true,
            attempts: 1,
        },
        Row {
            case: "a catcher on the States.ALL wildcard, which matches any known error",
            bounds: timeout_bound.clone(),
            handlers: catch(&["States.ALL"]),
            caught: true,
            attempts: 1,
        },
        Row {
            // "States.TaskFailed […] acts as a wildcard that matches any known error name
            // except States.Timeout", so this catcher does not answer for it.
            case: "a catcher on States.TaskFailed, the wildcard that excludes the timeout",
            bounds: timeout_bound.clone(),
            handlers: catch(&["States.TaskFailed"]),
            caught: false,
            attempts: 1,
        },
        Row {
            case: "a catcher naming the invoked service's own error instead",
            bounds: timeout_bound.clone(),
            handlers: catch(&["SQS.AmazonSQSException"]),
            caught: false,
            attempts: 1,
        },
        Row {
            // "HeartbeatSeconds […] if more time than the specified seconds elapses
            // between heartbeats from the task, this state fails with a States.Timeout
            // error name": the same error name, so the same catcher answers for it.
            case: "the heartbeat bound, with a catcher naming the error",
            bounds: json!({ "HeartbeatSeconds": BOUND_SECONDS }),
            handlers: catch(&[TIMEOUT_ERROR]),
            caught: true,
            attempts: 1,
        },
        Row {
            // The retrier answers FIRST: it makes the state's call again, carrying a
            // token of its own, and the catcher answers the attempt that follows it.
            case: "a retrier naming the error, and then the catcher",
            bounds: timeout_bound.clone(),
            handlers: json!({
                "Retry": [{
                    "ErrorEquals": [TIMEOUT_ERROR],
                    "IntervalSeconds": 0,
                    "MaxAttempts": 1,
                }],
                "Catch": [{ "ErrorEquals": [TIMEOUT_ERROR], "Next": CAUGHT_STATE }],
            }),
            caught: true,
            attempts: 2,
        },
        Row {
            case: "no handler at all, which leaves the failure to the execution",
            bounds: timeout_bound.clone(),
            handlers: json!({}),
            caught: false,
            attempts: 1,
        },
    ];

    let emulator = Emulator::new();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let queue_url = emulator.queue(&format!("{QUEUE_PREFIX}-{index}")).await;
        let execution_arn = emulator
            .started(
                &format!("bound-{index}"),
                &definition(&queue_url, &row.bounds, &row.handlers),
            )
            .await;
        let settled = emulator.settled(&execution_arn, case).await;
        let status = settled.get("status").and_then(Value::as_str);

        if row.caught {
            assert_eq!(
                status,
                Some("SUCCEEDED"),
                "case {case:?}: expected SUCCEEDED through the catcher, got {settled:?}"
            );
            // "When a state reports an error and it matches a catcher, the state's output
            // is the error output": the catcher's branch passes it on, so the execution
            // ends with the error the bound raised.
            let output: Value = serde_json::from_str(&string(&settled, "output"))
                .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"));
            assert_eq!(
                output.get("Error").and_then(Value::as_str),
                Some(TIMEOUT_ERROR),
                "case {case:?}: expected the error output to name {TIMEOUT_ERROR}, got {output:?}"
            );
            // The error output is the `{Error, Cause}` document of the failure the bound
            // raised: both keys are there, and the bound that simply ran out leaves
            // nothing to report in the second one — which is the empty string, not an
            // absent key. The optional `cause` MEMBER of the execution's own report stays
            // absent for it, because that is a member and this is the document.
            assert_eq!(
                output.get("Cause").and_then(Value::as_str),
                Some(""),
                "case {case:?}: expected the error output to carry an empty Cause, got {output:?}"
            );
        } else {
            assert_eq!(
                (status, settled.get("error").and_then(Value::as_str)),
                (Some("FAILED"), Some(TIMEOUT_ERROR)),
                "case {case:?}: expected FAILED with {TIMEOUT_ERROR}, got {settled:?}"
            );
        }

        // The bound is recorded as the timeout it is, once per attempt, and the state's
        // work was really handed over once per attempt too.
        let events = emulator.history(&execution_arn).await;
        assert_eq!(
            count_of(&events, EVENT_TASK_TIMED_OUT),
            row.attempts,
            "case {case:?}: expected {} {EVENT_TASK_TIMED_OUT} event(s), got {events:?}",
            row.attempts
        );
        assert_eq!(
            count_of(&events, EVENT_TASK_SUBMITTED),
            row.attempts,
            "case {case:?}: expected {} {EVENT_TASK_SUBMITTED} event(s), got {events:?}",
            row.attempts
        );
        let bodies = emulator.bodies(&queue_url, row.attempts).await;
        assert_eq!(
            bodies.len(),
            row.attempts,
            "case {case:?}: expected {} token(s) handed to the queue, got {bodies:?}",
            row.attempts
        );
        // Each attempt carries a token of its own: the retrier's attempt is not the one
        // the bound closed.
        let mut tokens: Vec<String> = bodies
            .iter()
            .map(|body| {
                let carried: Value = serde_json::from_str(body)
                    .unwrap_or_else(|why| panic!("case {case:?}: the body is JSON: {why}"));
                string(&carried, "token")
            })
            .collect();
        tokens.sort();
        tokens.dedup();
        assert_eq!(
            tokens.len(),
            row.attempts,
            "case {case:?}: expected {} distinct token(s), got {tokens:?}",
            row.attempts
        );
        // And every token the bound closed answers NOTHING afterwards, whatever the
        // state's handling did with the timeout: one token answers one task, and a task
        // whose bound the clock reached has been closed.
        for token in &tokens {
            for operation in ["SendTaskSuccess", "SendTaskFailure", "SendTaskHeartbeat"] {
                let answer = match operation {
                    "SendTaskSuccess" => json!({ "taskToken": token, "output": "{}" }),
                    _ => json!({ "taskToken": token }),
                };
                let refused = emulator.sfn_refusal(operation, &answer).await;
                assert_eq!(
                    refused,
                    (400, TASK_TIMED_OUT.to_string()),
                    "case {case:?}: expected {operation} on a closed token to be refused \
                     {TASK_TIMED_OUT}, got {refused:?}"
                );
            }
        }
    }
}
