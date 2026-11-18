// SPDX-License-Identifier: CC0-1.0

//! The event-publication CALLBACK round trip over the PRODUCTION assembly: the emulator as
//! `core/router/src/lib.rs` builds it, with every service mounted on one engine, rather than a
//! hand-picked few.
//!
//! RULE: a `Task` state whose `Resource` names an optimized integration suffixed
//! `.waitForTaskToken` mints a task token as the state is entered, substitutes it wherever the
//! state's own `Parameters` quote the context object's `$$.Task.Token`, makes the integration
//! call ONCE with that rendered request — for the event-publication integration, an EventBridge
//! `PutEvents` onto the bus its `EventBusName` addresses, as an envelope attributed to the
//! publishing account and Region — and then HOLDS the execution `RUNNING`: what finishes the
//! state is the `SendTask*` quoting that token, whoever makes it.
//!
//! What this suite adds to its neighbours, rather than repeats: the Step Functions crate's
//! `tests/all/events_put_events_callback_round_trip.rs` owns the TABLE over this rule — how the
//! entry addresses its bus, where in the rendered document the token sits, which `SendTask*`
//! answers it and which then re-uses it — over four services it registers itself, and
//! `all/sfn_task_target_authorization.rs` owns the execution role the call is made as. Neither
//! reaches the assembled emulator: every service registered together, each claiming only the
//! requests scoped to its own signing name, with the Step Functions walk's in-process
//! `PutEvents` and EventBridge's delivery to a queue target crossing that full routing table.
//! So this suite drives ONE configuration of the rule end to end through
//! `aws_emulator_router::build_router`, and asserts nothing about the row space its neighbour
//! covers.
//!
//! The work that answers the token is this suite itself, reading the token out of the envelope
//! EventBridge delivered to the rule's queue target: the rule is about the token's redemption,
//! not about which runtime redeems it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The reader of an awsJson error body's wire code, and the Step Functions harness's role,
// statement and member readers: `tests/all/` is one crate root, so this suite borrows all of
// them rather than carrying a second copy.
use crate::logs_delivery_source_cloudfront_resource::error_code;
use crate::sfn_task_target_authorization::{allow, logs_only, role, string, ACCOUNT, REGION};

/// The `source` and `detail-type` the Task publishes under, and the constant its rendered
/// `Detail` carries so the rule matches this publication and nothing else on the bus.
const SOURCE: &str = "sim.sfn";
const DETAIL_TYPE: &str = "task.requested";
const NONCE: &str = "assembled-emulator";

/// The reserved context-object path the state's `Parameters` quote to ask for that task's own
/// token, and the marker that opens every context-object path: a rendered document still
/// carrying either is one where the substitution did not happen.
const TOKEN_PATH: &str = "$$.Task.Token";
const CONTEXT_MARKER: &str = "$$";

/// The status an execution sits in while its callback task is outstanding, and the one it
/// reaches when that task is answered with a success (`ExecutionStatus`).
const RUNNING: &str = "RUNNING";
const SUCCEEDED: &str = "SUCCEEDED";

/// What every `SendTask*` answers for a token whose task is no longer outstanding: the modelled
/// `TaskTimedOut` ("the task token has either expired or the task associated with the token has
/// already been closed", `smithy_query(query="operation",operation="SendTaskSuccess")`), carried
/// as a 400.
const SPENT_TOKEN_STATUS: u16 = 400;
const SPENT_TOKEN_CODE: &str = "TaskTimedOut";

/// The principal EventBridge delivers to a resource-policy-governed target as: "for Lambda
/// functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based
/// policies" (`PutTargets`), so a queue that has granted it nothing receives nothing.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The definition-level allowance the callback task is given, which it must not need: the round
/// trip ends on the answer to its token, long before it.
const TASK_TIMEOUT_SECONDS: u64 = 240;

/// How long one poll waits before looking again, and how long this suite waits for work the
/// clock and the assembled services owe: generous, so a loaded host cannot decide the outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const PATIENCE: Duration = Duration::from_secs(180);

/// The emulator as it is ASSEMBLED, on one engine: every service `core/router/src/lib.rs` mounts,
/// claiming the requests scoped to its own signing name.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// One `awsJson` call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Step Functions call, as its status and parsed body — the refusal half of the rule is read
/// from a call that is not expected to be served.
async fn states(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "states", "AWSStepFunctions", "1.0", operation, body).await
}

/// One Step Functions call that must answer 200.
async fn states_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = states(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200 from Step Functions, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call that must answer 200.
async fn events_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200 from EventBridge, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call that must answer 200.
async fn sqs_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200 from Amazon SQS, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name` carrying the grant a rule's target needs; answers its url and the ARN a
/// rule names it by.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs_ok(router, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = string(&created, "QueueUrl");
    let attributes = sqs_ok(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected the QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs_ok(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// The one envelope the queue at `url` is delivered, looked for until the publication the Task
/// makes has been routed to it.
async fn delivered_envelope(router: &MultiServiceRouter, url: &str) -> Value {
    let began = Instant::now();
    let mut held: Vec<Value> = Vec::new();
    loop {
        let received = sqs_ok(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 0 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default();
            held.push(serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
                panic!("the delivered body must be a JSON envelope: {body} ({error})")
            }));
        }
        if let [only] = held.as_slice() {
            return only.clone();
        }
        assert!(
            held.is_empty(),
            "one publication must reach the matching rule once, got {held:?}"
        );
        assert!(
            began.elapsed() < PATIENCE,
            "the Task's publication must reach the matching rule within {PATIENCE:?}, nothing \
             arrived"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// `execution` once it has left [`RUNNING`].
async fn settled(router: &MultiServiceRouter, execution: &str) -> Value {
    let began = Instant::now();
    loop {
        let reported = states_ok(
            router,
            "DescribeExecution",
            &json!({ "executionArn": execution }),
        )
        .await;
        if reported.get("status").and_then(Value::as_str) != Some(RUNNING) {
            return reported;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the answered callback task must take the execution out of {RUNNING} within \
             {PATIENCE:?}, last saw {reported}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// On the assembled emulator: the callback `Task`'s rendered entry is published onto the custom
/// bus it names and routed to the rule's queue target as an envelope attributed to this account
/// and Region, carrying the token the state minted in place of the context-object path; the
/// execution waits at the state until that token is answered, the answer is its output, and the
/// spent token is then refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_assembled_emulator_serves_the_event_publication_callback_round_trip() {
    let router = emulator();

    let bus_arn = string(
        &events_ok(&router, "CreateEventBus", &json!({ "Name": "assembled-bus" })).await,
        "EventBusArn",
    );
    let (queue_url, queue_arn) = given_queue(&router, "assembled-callback-queue").await;
    events_ok(
        &router,
        "PutRule",
        &json!({
            "Name": "assembled-callback-rule",
            "EventBusName": "assembled-bus",
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": { "nonce": [NONCE] },
            })
            .to_string(),
        }),
    )
    .await;
    let wired = events_ok(
        &router,
        "PutTargets",
        &json!({
            "Rule": "assembled-callback-rule",
            "EventBusName": "assembled-bus",
            "Targets": [{ "Id": "the-queue", "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        wired.get("FailedEntryCount").and_then(Value::as_i64),
        Some(0),
        "the rule's queue target must be accepted, got {wired}"
    );

    // The publication is a `Task` state's integration call, so it is made as the state machine's
    // execution role: on the assembled emulator IAM holds that role and its documents have to
    // allow the operation's own action.
    let role_arn = role(
        &router,
        "AssembledEventsCallbackRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow("events:PutEvents", "*")],
        }),
    )
    .await;

    let definition = json!({
        "StartAt": "RequestWork",
        "States": {
            "RequestWork": {
                "Type": "Task",
                "Resource": "arn:aws:states:::events:putEvents.waitForTaskToken",
                "Parameters": {
                    "Entries": [{
                        "EventBusName": bus_arn,
                        "Source": SOURCE,
                        "DetailType": DETAIL_TYPE,
                        "Detail": {
                            "taskToken.$": TOKEN_PATH,
                            "job.$": "$.job",
                            "nonce": NONCE,
                        },
                    }],
                },
                "TimeoutSeconds": TASK_TIMEOUT_SECONDS,
                "End": true,
            },
        },
    })
    .to_string();
    let machine = string(
        &states_ok(
            &router,
            "CreateStateMachine",
            &json!({
                "name": "assembled-callback-machine",
                "definition": definition,
                "roleArn": role_arn,
            }),
        )
        .await,
        "stateMachineArn",
    );
    let job = "job-assembled-0";
    let execution = string(
        &states_ok(
            &router,
            "StartExecution",
            &json!({
                "stateMachineArn": machine,
                "name": "only",
                "input": json!({ "job": job }).to_string(),
            }),
        )
        .await,
        "executionArn",
    );

    // The entry the state rendered crossed the whole routing table and reached the rule's
    // target, attributed to the account and Region it was published in.
    let envelope = delivered_envelope(&router, &queue_url).await;
    assert_eq!(
        (
            envelope.get("source").and_then(Value::as_str),
            envelope.get("detail-type").and_then(Value::as_str),
            envelope.get("account").and_then(Value::as_str),
            envelope.get("region").and_then(Value::as_str),
        ),
        (Some(SOURCE), Some(DETAIL_TYPE), Some(ACCOUNT), Some(REGION)),
        "expected the Task's own source and detail type on an envelope attributed to \
         {ACCOUNT}/{REGION}, got {envelope}"
    );
    let detail = envelope
        .get("detail")
        .unwrap_or_else(|| panic!("expected a detail in {envelope}"))
        .clone();
    assert_eq!(
        detail.get("job").and_then(Value::as_str),
        Some(job),
        "expected the job the execution was started with, got {detail}"
    );
    let token = detail["taskToken"]
        .as_str()
        .unwrap_or_else(|| panic!("expected the substituted task token in {detail}"))
        .to_string();
    assert!(
        !token.is_empty() && token != TOKEN_PATH && !detail.to_string().contains(CONTEXT_MARKER),
        "expected the context object's token in place of {TOKEN_PATH:?}, got {detail}"
    );

    // The integration call's own answer did not finish the state: the execution is still waiting
    // on the token that has just travelled out.
    let waiting = states_ok(
        &router,
        "DescribeExecution",
        &json!({ "executionArn": &execution }),
    )
    .await;
    assert_eq!(
        waiting.get("status").and_then(Value::as_str),
        Some(RUNNING),
        "a published callback task leaves its execution {RUNNING} until its token is answered, \
         got {waiting}"
    );

    // Answering the token out of the delivered envelope is what settles the execution, and what
    // the answer carried is its output.
    let answer = json!({ "job": job, "nonce": NONCE }).to_string();
    states_ok(
        &router,
        "SendTaskSuccess",
        &json!({ "taskToken": &token, "output": &answer }),
    )
    .await;
    let reported = settled(&router, &execution).await;
    assert_eq!(
        (
            reported.get("status").and_then(Value::as_str),
            reported.get("output").and_then(Value::as_str),
        ),
        (Some(SUCCEEDED), Some(answer.as_str())),
        "expected {SUCCEEDED} with exactly the string the answer carried ({answer:?}), got \
         {reported}"
    );

    // And that token is spent: the next call quoting it is refused.
    let (status, refusal) = states(
        &router,
        "SendTaskSuccess",
        &json!({ "taskToken": &token, "output": json!({ "late": true }).to_string() }),
    )
    .await;
    let code = error_code(&refusal);
    assert_eq!(
        (status, code.as_str()),
        (SPENT_TOKEN_STATUS, SPENT_TOKEN_CODE),
        "expected the spent token to be refused {SPENT_TOKEN_STATUS} {SPENT_TOKEN_CODE}, got \
         {status} with {refusal}"
    );
}
