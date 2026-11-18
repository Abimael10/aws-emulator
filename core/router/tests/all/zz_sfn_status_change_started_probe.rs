// SPDX-License-Identifier: CC0-1.0

//! An execution's status changes reach an EventBridge rule's SQS target on the emulator a
//! customer STARTS — the built binary, request authentication on, every call over TLS, read
//! with the long poll a client watching a queue uses.
//!
//! The RULE the legs assert, one level above the recorded case:
//!
//! > Every status an execution of a STANDARD state machine MOVES INTO is published onto the
//! > account's DEFAULT EventBridge bus as `source` `aws.states` / `detail-type`
//! > `Step Functions Execution Status Change` and delivered to the targets of every rule
//! > that matches it — on the emulator as a client STARTS it, not only on a router a test
//! > assembles in its own process, and whether the client's receive is issued after the
//! > transition or is ALREADY IN FLIGHT while it falls due.
//!
//! The two legs are that axis: one reads the queue after polling the execution to its
//! terminal status, the other has its long poll already running while the execution is
//! started and the transition falls due. Everything the emulator logged is quoted by a
//! failure, which is where a delivery it could not make says so.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

const SFN_SOURCE: &str = "aws.states";
const EXECUTION_STATUS_CHANGE: &str = "Step Functions Execution Status Change";
const DEFAULT_BUS: &str = "default";
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
/// How long a leg waits for what it is watching for before it gives up, and how long it
/// leaves between looks.
///
/// Generous: every wait is for a CONDITION — the execution reaching a terminal status, the
/// envelope arriving — so a host running this beside the rest of the workspace's tests
/// spends the span instead of failing on it.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(250);

/// One RPC call over the started emulator's TLS endpoint, answered as its parsed body.
fn rpc(
    emulator: &Started,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, answered) = emulator.call(
        signing_name,
        "POST",
        "/",
        &[
            (
                "content-type",
                &format!("application/x-amz-json-{json_version}"),
            ),
            ("x-amz-target", &format!("{target_prefix}.{operation}")),
        ],
        body.to_string().into_bytes(),
    );
    let parsed = serde_json::from_str::<Value>(&answered).unwrap_or(Value::String(answered));
    (status, parsed)
}

fn sfn(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        emulator,
        "states",
        "AWSStepFunctions",
        "1.0",
        operation,
        body,
    )
}

fn events(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "events", "AWSEvents", "1.1", operation, body)
}

fn sqs(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "sqs", "AmazonSQS", "1.0", operation, body)
}

fn text(body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE: the status changes of an execution reach the queue a matching rule targets on the
/// started emulator, both for a receive issued after the transition and for one already in
/// flight while it falls due.
#[test]
fn status_changes_reach_their_rules_queue_on_the_started_emulator() {
    let emulator = Started::start();
    let mut notes: Vec<String> = Vec::new();

    // The consumer: a queue admitting EventBridge behind a rule on every execution status
    // change.
    let (_, created) = sqs(&emulator, "CreateQueue", &json!({"QueueName": "probe-status"}));
    let url = text(&created, "/QueueUrl");
    notes.push(format!("CreateQueue -> {created}"));
    let (_, attributes) = sqs(
        &emulator,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    );
    let queue_arn = text(&attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &queue_arn,
        }],
    })
    .to_string();
    let (_, set) = sqs(
        &emulator,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    );
    notes.push(format!("SetQueueAttributes -> {set}"));
    let (_, put_rule) = events(
        &emulator,
        "PutRule",
        &json!({
            "Name": "probe-status",
            "EventBusName": DEFAULT_BUS,
            "EventPattern": json!({
                "source": [SFN_SOURCE],
                "detail-type": [EXECUTION_STATUS_CHANGE],
            })
            .to_string(),
            "State": "ENABLED",
        }),
    );
    notes.push(format!("PutRule -> {put_rule}"));
    let (_, targeted) = events(
        &emulator,
        "PutTargets",
        &json!({
            "Rule": "probe-status",
            "EventBusName": DEFAULT_BUS,
            "Targets": [{"Id": "t1", "Arn": &queue_arn}],
        }),
    );
    notes.push(format!("PutTargets -> {targeted}"));

    // The workflow.
    let (_, machine) = sfn(
        &emulator,
        "CreateStateMachine",
        &json!({
            "name": "probe-machine",
            "definition": json!({
                "StartAt": "Ok",
                "States": {"Ok": {"Type": "Pass", "Result": {"a": 1}, "End": true}},
            })
            .to_string(),
            "roleArn": "arn:aws:iam::000000000000:role/StatesExecutionRole",
            "type": "STANDARD",
        }),
    );
    notes.push(format!("CreateStateMachine -> {machine}"));
    let machine_arn = text(&machine, "/stateMachineArn");
    let (_, started) = sfn(
        &emulator,
        "StartExecution",
        &json!({
            "stateMachineArn": &machine_arn,
            "name": "probe-run",
            "input": r#"{"who":"world"}"#,
        }),
    );
    notes.push(format!("StartExecution -> {started}"));
    let execution_arn = text(&started, "/executionArn");

    // The client polls the execution to its terminal status, as a client watching a
    // workflow does.
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let (_, described) = sfn(
            &emulator,
            "DescribeExecution",
            &json!({"executionArn": &execution_arn}),
        );
        last = described;
        if last["status"].as_str() != Some("RUNNING") {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    notes.push(format!("DescribeExecution -> {last}"));

    // And then reads the queue the rule targets, with the long poll a client uses.
    let giving_up_at = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    while Instant::now() < giving_up_at {
        let (status, received) = sqs(
            &emulator,
            "ReceiveMessage",
            &json!({"QueueUrl": &url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 5}),
        );
        notes.push(format!("ReceiveMessage {status} -> {received}"));
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                sqs(
                    &emulator,
                    "DeleteMessage",
                    &json!({"QueueUrl": &url, "ReceiptHandle": handle}),
                );
            }
            arrived.push(
                serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                    .unwrap_or(Value::Null),
            );
        }
        if arrived.iter().any(|envelope| {
            !matches!(
                envelope["detail"]["status"].as_str(),
                None | Some("RUNNING")
            )
        }) {
            break;
        }
    }

    let served = arrived.iter().any(|envelope| {
        !matches!(
            envelope["detail"]["status"].as_str(),
            None | Some("RUNNING")
        )
    });
    assert!(
        served,
        "the started emulator delivered no terminal status change.\ncalls:\n{}\n\narrived: \
         {arrived:?}\n\nthe emulator's log:\n{}",
        notes.join("\n"),
        emulator.log()
    );

    // And the same scenario again, with the client's drain running CONCURRENTLY with the
    // execution it watches, through the long poll a client watching a queue uses: a drain
    // that is in flight while the transition falls due must not keep the transition from
    // being delivered.
    let (_, created) = sqs(
        &emulator,
        "CreateQueue",
        &json!({"QueueName": "probe-status-live"}),
    );
    let live_url = text(&created, "/QueueUrl");
    let (_, attributes) = sqs(
        &emulator,
        "GetQueueAttributes",
        &json!({"QueueUrl": &live_url, "AttributeNames": ["QueueArn"]}),
    );
    let live_arn = text(&attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &live_arn,
        }],
    })
    .to_string();
    sqs(
        &emulator,
        "SetQueueAttributes",
        &json!({"QueueUrl": &live_url, "Attributes": {"Policy": policy}}),
    );
    events(
        &emulator,
        "PutRule",
        &json!({
            "Name": "probe-status-live",
            "EventBusName": DEFAULT_BUS,
            "EventPattern": json!({"source": [SFN_SOURCE]}).to_string(),
            "State": "ENABLED",
        }),
    );
    events(
        &emulator,
        "PutTargets",
        &json!({
            "Rule": "probe-status-live",
            "EventBusName": DEFAULT_BUS,
            "Targets": [{"Id": "t1", "Arn": &live_arn}],
        }),
    );

    let live: Vec<Value> = std::thread::scope(|scope| {
        let drained = scope.spawn(|| {
            let giving_up_at = Instant::now() + DEADLINE;
            let mut seen: Vec<Value> = Vec::new();
            while Instant::now() < giving_up_at {
                let (_, received) = sqs(
                    &emulator,
                    "ReceiveMessage",
                    &json!({
                        "QueueUrl": &live_url,
                        "MaxNumberOfMessages": 10,
                        "WaitTimeSeconds": 20,
                    }),
                );
                for message in received["Messages"].as_array().cloned().unwrap_or_default() {
                    if let Some(handle) = message["ReceiptHandle"].as_str() {
                        sqs(
                            &emulator,
                            "DeleteMessage",
                            &json!({"QueueUrl": &live_url, "ReceiptHandle": handle}),
                        );
                    }
                    seen.push(
                        serde_json::from_str::<Value>(
                            message["Body"].as_str().unwrap_or_default(),
                        )
                        .unwrap_or(Value::Null),
                    );
                }
                if seen.iter().any(|envelope| {
                    !matches!(
                        envelope["detail"]["status"].as_str(),
                        None | Some("RUNNING")
                    )
                }) {
                    break;
                }
            }
            seen
        });
        let (_, machine) = sfn(
            &emulator,
            "CreateStateMachine",
            &json!({
                "name": "probe-machine-live",
                "definition": json!({
                    "StartAt": "Ok",
                    "States": {"Ok": {"Type": "Pass", "End": true}},
                })
                .to_string(),
                "roleArn": "arn:aws:iam::000000000000:role/StatesExecutionRole",
                "type": "STANDARD",
            }),
        );
        let machine_arn = text(&machine, "/stateMachineArn");
        sfn(
            &emulator,
            "StartExecution",
            &json!({"stateMachineArn": &machine_arn, "name": "probe-run-live"}),
        );
        drained.join().expect("the drain thread runs to completion")
    });
    let served_live = live.iter().any(|envelope| {
        !matches!(
            envelope["detail"]["status"].as_str(),
            None | Some("RUNNING")
        )
    });
    assert!(
        served_live,
        "a drain already in flight saw no terminal status change: {live:?}\n\nthe emulator's \
         log:\n{}",
        emulator.log()
    );
}
