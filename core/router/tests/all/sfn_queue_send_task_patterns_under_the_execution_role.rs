// SPDX-License-Identifier: CC0-1.0

//! What the state machine's EXECUTION ROLE decides about the queue send a `Task` state asks
//! for, over every integration PATTERN it can ask for one in — driven over each service's own
//! wire protocol through the production `MultiServiceRouter` with Step Functions, IAM and
//! Amazon SQS mounted on ONE engine, the shape `core/router/src/lib.rs` mounts them in, so the
//! documents the call is judged against are the ones IAM's own handlers wrote and an allowed
//! send really reaches the queue.
//!
//! RULE: a `Task` state's integration call is made as the state machine's `roleArn` — "the
//! Amazon Resource Name (ARN) of the IAM role to use for this state machine"
//! (`CreateStateMachine$roleArn`) — and whether the documents in force on that role allow the
//! action the invoked operation publishes, on the ARN of the resource the request addresses, is
//! decided BEFORE the request goes anywhere, whatever integration pattern the `Resource` asks
//! the call in. A call no document in force allows is therefore not made at all: the task fails
//! with the invoked service's own access-denied refusal, the execution settles `FAILED`, and
//! the resource is untouched — so a callback (`.waitForTaskToken`) task hands no token over,
//! claims no submission, and waits for no answer. Where the same role IS granted the send, the
//! very same definition's call is made: the message arrives on the queue, and a callback task's
//! message carries the token the state minted, which is what tells a call that was never made
//! from one that happened.
//!
//! The refusal arrives under Amazon SQS's BASE SDK exception: `SendMessage` models no
//! access-denied error of its own (`smithy_query(query="operation",operation="SendMessage",
//! model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/
//! com-amazonaws-sqs-amazonsqs.json")`), so no client carries a class for the code such an
//! answer names and the base exception is what the code deserializes into. Its message is the
//! whole sentence the refusal carries, which is what this suite holds it to: the fixed text
//! around the identity the call would have been made as, the action as that service's own
//! front door words it, the ARN the request addressed, and the metadata tail the client serving
//! this integration writes — only the id of the refused request is one run's own value, so it
//! is masked rather than fixed.
//!
//! The rows differ along what the rule generalises over: the integration pattern the `Resource`
//! asks the send in (request-response, and the callback `.waitForTaskToken` one) and what the
//! role's document says about the send (nothing about it, the action over ANOTHER queue, the
//! action over the very queue addressed) — the granted rows being the positive controls that
//! the same call is made, and the message really delivered, when it is allowed. Each pattern is
//! a test of its own, over an emulator of its own, so neither waits on the other's work.
//!
//! What the neighbours pin and this one does not repeat: `all/sfn_task_target_authorization.rs`
//! pins WHICH resource the access request is over, `all/sfn_task_integration_grant_scope.rs`
//! the grant's SCOPE over the request-response send, and
//! `all/sfn_sqs_callback_token_lifecycle.rs` what a callback token does once it is handed over.
//! What only this suite reaches is the decision over the PATTERN the `Resource` asks the send
//! in, and with it that a denied callback task hands nothing over and the refusal's whole
//! sentence.
//!
//! The harness — the services on one engine, the role, the queue, the state machine and the
//! execution reader — is `all/sfn_task_target_authorization.rs`'s, and `tests/all/main.rs` is
//! this binary's crate root, so it is reached through `crate::sfn_task_target_authorization`
//! rather than restated here.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sfn_task_target_authorization::{
    allow, emulator, logs_only, machine, masked, queue, ran, refusal_metadata, role,
    session_principal, sfn, sqs, string, waiting, FORBIDDEN, SQS_REFUSAL_CODE,
};

/// The IAM action a send is subject to, as a grant spells it: the action Amazon SQS publishes
/// for `SendMessage`.
const SEND_GRANT: &str = "sqs:SendMessage";

/// The same action as the REFUSAL spells it: Amazon SQS's own authorization layer reports the
/// action it refused in lower case, and the sentence is that service's own.
const REFUSED_ACTION: &str = "sqs:sendmessage";

/// The error a `Task` state reports when the role may not make the send: Amazon SQS's base SDK
/// exception, which is what a refusal `SendMessage` models no error for arrives under.
const REFUSAL_ERROR: &str = "SQS.AmazonSQSException";

/// The name the client this integration is served by gives the invoked service, which is what
/// its rendering of the exception reports the service under.
const REFUSED_SERVICE: &str = "AmazonSQS";

/// The history events one attempt writes
/// (`smithy_query(query="shape",shape="HistoryEventType")`): the one a refused task closes
/// with, and the one a call that really handed its work over claims.
const TASK_FAILED: &str = "TaskFailed";
/// See [`TASK_FAILED`].
const TASK_SUBMITTED: &str = "TaskSubmitted";

/// The document this suite answers a granted callback row's token with, which is then that
/// execution's own output.
const ANSWERED: &str = r#"{"answered":"from outside the send"}"#;

/// How long one poll of a condition the work owes sleeps between reads.
const POLL: Duration = Duration::from_millis(50);

/// A ceiling on patience, never a span anything here counts on: a row waits for the CONDITION
/// it is about — a message on the queue, an execution out of `RUNNING` — and how long the host
/// takes to get there is no row's verdict.
const PATIENCE: Duration = Duration::from_secs(120);

/// Which integration PATTERN a `Resource` asks the send in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pattern {
    /// The call answers the task: request-response.
    RequestResponse,
    /// `.waitForTaskToken`: the call carries a token, and whoever holds it answers the task.
    Callback,
}

impl Pattern {
    /// The `Resource` a row of this pattern names, which is the optimized integration with the
    /// suffix the pattern asks for.
    fn resource(self) -> &'static str {
        match self {
            Self::RequestResponse => "arn:aws:states:::sqs:sendMessage",
            Self::Callback => "arn:aws:states:::sqs:sendMessage.waitForTaskToken",
        }
    }
}

/// What one row's document says about the send it is judged on.
#[derive(Clone, Copy, Debug)]
enum Says {
    /// Nothing about any send at all.
    Nothing,
    /// The published action, over ANOTHER queue of the same account.
    GrantOnAnother,
    /// The published action, over the very queue the request addresses.
    GrantHere,
}

/// One row: the case, the pattern the `Resource` asks the send in, and what the role's document
/// says about it.
struct Row {
    case: &'static str,
    pattern: Pattern,
    says: Says,
}

impl Row {
    /// Whether what the document says leaves the send allowed: only a grant over the very
    /// resource the request addresses does.
    fn allowed(&self) -> bool {
        matches!(self.says, Says::GrantHere)
    }

    /// The statements this row's role carries: the log writes every execution role has,
    /// whatever else it does, plus what this row says about the send.
    fn statements(&self, addressed: &str, another: &str) -> Value {
        let mut statements = vec![logs_only()];
        match self.says {
            Says::Nothing => {}
            Says::GrantOnAnother => statements.push(allow(SEND_GRANT, another)),
            Says::GrantHere => statements.push(allow(SEND_GRANT, addressed)),
        }
        json!({ "Version": "2012-10-17", "Statement": Value::Array(statements) })
    }

    /// The request the `Task` state carries. The body is a JSON document — the member the
    /// integration documents as one — so a callback row can hand the token the pattern minted
    /// for the state into it beside the row's own note, and a delivered message can be read
    /// back for both.
    fn parameters(&self, queue_url: &str, note: &str) -> Value {
        let mut body = json!({ "note": note });
        if self.pattern == Pattern::Callback {
            // "Task tokens are generated by Step Functions [...] in the context object when a
            // workflow enters a task state": the token of THIS state, which is what a message
            // on the queue proves the call really carried.
            body["token.$"] = json!("$$.Task.Token");
        }
        json!({ "QueueUrl": queue_url, "MessageBody": body })
    }
}

/// Every thing a document can say about the send, asked of the REQUEST-RESPONSE pattern.
fn request_response_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a request-response send, under a role that says nothing about it",
            pattern: Pattern::RequestResponse,
            says: Says::Nothing,
        },
        Row {
            case: "a request-response send, under a role granted the send on another queue",
            pattern: Pattern::RequestResponse,
            says: Says::GrantOnAnother,
        },
        Row {
            case: "a request-response send, under a role granted the send on this queue",
            pattern: Pattern::RequestResponse,
            says: Says::GrantHere,
        },
    ]
}

/// The same things, asked of the CALLBACK pattern. A test of its own, so neither pattern's
/// rows wait on the other's work.
fn callback_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a callback send, under a role that says nothing about it",
            pattern: Pattern::Callback,
            says: Says::Nothing,
        },
        Row {
            case: "a callback send, under a role granted the send on another queue",
            pattern: Pattern::Callback,
            says: Says::GrantOnAnother,
        },
        Row {
            case: "a callback send, under a role granted the send on this queue",
            pattern: Pattern::Callback,
            says: Says::GrantHere,
        },
    ]
}

/// Start one execution and answer its ARN, without waiting for it to settle: a callback row's
/// task waits for an answer, so the row has work to do while the execution is still `RUNNING`.
async fn started(router: &MultiServiceRouter, machine_arn: &str) -> String {
    let answered = sfn(
        router,
        "StartExecution",
        &json!({ "stateMachineArn": machine_arn, "name": "only" }),
    )
    .await;
    string(&answered, "executionArn")
}

/// The execution `execution_arn`, looked at again until it has left `RUNNING`.
async fn settled(router: &MultiServiceRouter, execution_arn: &str, case: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = sfn(
            router,
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

/// The bodies the queue at `url` holds, looked at again until `wanted` of them have arrived or
/// patience runs out. How many really arrived is the caller's own assertion.
async fn bodies(router: &MultiServiceRouter, url: &str, wanted: usize) -> Vec<String> {
    let began = Instant::now();
    let mut collected: Vec<String> = Vec::new();
    loop {
        let received = sqs(
            router,
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

/// Every event of `execution_arn`'s history.
async fn history(router: &MultiServiceRouter, execution_arn: &str) -> Vec<Value> {
    let answered = sfn(
        router,
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

/// The JSON document a delivered message's body carries.
fn delivered(body: &str, case: &str) -> Value {
    serde_json::from_str(body)
        .unwrap_or_else(|why| panic!("case {case:?}: the body must be JSON, got {body:?} ({why})"))
}

/// RULE, over the request-response pattern: the role's documents decide the send before it is
/// made, so a denied row's queue stays untouched and its execution fails with the refusal's
/// whole sentence, while a granted row's message really arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_execution_roles_decision_is_taken_before_a_request_response_send() {
    judged(request_response_rows()).await;
}

/// RULE, over the callback pattern: the same decision, taken before the token is handed over —
/// so a denied row claims no submission and waits for no answer, while a granted row's message
/// carries the token that then finishes the execution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_execution_roles_decision_is_taken_before_a_callback_sends_token_is_handed_over() {
    judged(callback_rows()).await;
}

/// Drive `rows` through one emulator of their own, each row on a queue and a role of its own.
async fn judged(rows: Vec<Row>) {
    let router = emulator();
    // The queue a document naming ANOTHER resource names, which no row's task addresses.
    let (_, another_arn) = queue(&router, "send-pattern-bystander").await;

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        // A queue of its own per row, so what it holds afterwards is what THIS row's task did
        // with it.
        let (queue_url, queue_arn) = queue(&router, &format!("send-pattern-{index}")).await;
        let role_arn = role(
            &router,
            &format!("QueueSendPatternRole{index}"),
            &row.statements(&queue_arn, &another_arn),
        )
        .await;
        // The note each row sends is its own, so a message found on a queue belongs to the row
        // that was allowed to send it.
        let note = format!("the-work-{index}");
        let machine_arn = machine(
            &router,
            &format!("send-pattern-{index}"),
            row.pattern.resource(),
            &row.parameters(&queue_url, &note),
            &role_arn,
        )
        .await;

        if !row.allowed() {
            // A DENIED row settles on its own: the refusal is the task's outcome, so there is
            // nothing to answer and nothing to wait for.
            let (execution_arn, described) = ran(&router, &machine_arn, case).await;
            assert_eq!(
                (
                    described.get("status").and_then(Value::as_str),
                    described.get("error").and_then(Value::as_str),
                ),
                (Some("FAILED"), Some(REFUSAL_ERROR)),
                "case {case:?}: expected the refusal to end the execution, got {described:?}"
            );
            let events = history(&router, &execution_arn).await;
            let types: Vec<&str> = events
                .iter()
                .filter_map(|event| event.get("type").and_then(Value::as_str))
                .collect();
            assert!(
                types.contains(&TASK_FAILED),
                "case {case:?}: expected the attempt to close with {TASK_FAILED}, got {types:?}"
            );
            // A call that was never made handed nothing over, so no submission is claimed for
            // it: a denied callback task waits for an answer nothing would ever send.
            assert!(
                !types.contains(&TASK_SUBMITTED),
                "case {case:?}: a send that was never made claims no submission, got {types:?}"
            );
            let details = events
                .iter()
                .find_map(|event| event.get("taskFailedEventDetails"))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected taskFailedEventDetails in {events:?}")
                });
            let cause = string(details, "cause");
            // The WHOLE sentence the invoked service refuses the call with: the session of the
            // role the call would have been made as, the action as that service words it, the
            // ARN the request addressed, why the documents withheld it, and the metadata tail
            // the serving client writes. Only the id of the refused request is one run's own
            // value, so it is masked.
            let principal = session_principal(&role_arn, &cause);
            let expected = format!(
                "User: {principal} is not authorized to perform: {REFUSED_ACTION} on resource: \
                 {queue_arn} because no identity-based policy allows the {REFUSED_ACTION} \
                 action{}",
                refusal_metadata(
                    row.pattern.resource(),
                    REFUSED_SERVICE,
                    SQS_REFUSAL_CODE,
                    FORBIDDEN,
                )
            );
            assert_eq!(
                masked(&cause),
                expected,
                "case {case:?}: expected the refusal {expected:?}, got {cause:?}"
            );
            // And the execution's own cause is that refusal too, so a caller that never reads
            // the history sees why the workflow stopped.
            let settled_cause = string(&described, "cause");
            assert!(
                settled_cause.contains(REFUSED_ACTION) && settled_cause.contains(&queue_arn),
                "case {case:?}: expected the execution's cause to name the refused send, got \
                 {settled_cause:?}"
            );
            // Read ONCE, and that is enough: the execution has already settled, so a send
            // that had happened would have left its message here.
            let left = waiting(&router, &queue_url).await;
            assert_eq!(
                left, 0,
                "case {case:?}: expected the queue the role may not send to to be untouched, got \
                 {left} message(s)"
            );
            continue;
        }

        // A GRANTED row: the same definition's call is made, so the message arrives.
        let execution_arn = started(&router, &machine_arn).await;
        let arrived = bodies(&router, &queue_url, 1).await;
        assert_eq!(
            arrived.len(),
            1,
            "case {case:?}: expected the one message the task sent, got {arrived:?}"
        );
        let body = delivered(&arrived[0], case);
        assert_eq!(
            body.get("note").and_then(Value::as_str),
            Some(note.as_str()),
            "case {case:?}: expected the body this row's task sent, got {body}"
        );

        match row.pattern {
            // A request-response row's send answers its own task, so the execution settles
            // without anything else happening.
            Pattern::RequestResponse => {
                let described = settled(&router, &execution_arn, case).await;
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the granted send to be made, got {described:?}"
                );
                assert!(
                    body.get("token").is_none(),
                    "case {case:?}: expected no token in a request-response body, got {body}"
                );
            }
            // A callback row's send HANDED a token over, and that token is what answers the
            // task: the message carries it, so answering it finishes the execution with the
            // document the answer carried.
            Pattern::Callback => {
                let token = body
                    .get("token")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected the token the send carried in {body}")
                    })
                    .to_string();
                sfn(
                    &router,
                    "SendTaskSuccess",
                    &json!({ "taskToken": token, "output": ANSWERED }),
                )
                .await;
                let described = settled(&router, &execution_arn, case).await;
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the answered token to finish the execution, got \
                     {described:?}"
                );
                let output = delivered(&string(&described, "output"), case);
                assert_eq!(
                    output,
                    delivered(ANSWERED, case),
                    "case {case:?}: expected the answer the token was finished with, got {output}"
                );
            }
        }
    }
}
