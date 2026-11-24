// SPDX-License-Identifier: CC0-1.0

//! The whole Amazon SQS -> AWS Lambda -> Amazon SQS relay a client WIRES BY HAND, driven
//! against the emulator a customer STARTS: the built binary, on its own port, with request
//! authentication on, reached over TLS with SigV4-signed calls.
//!
//! The rule pinned here: **a mapping reporting `Enabled` over a queue receives from that queue
//! on its own and invokes the mapped function with the `Records` envelope of the batch it took
//! — whatever the function is configured for, the `Timeout` a create that states none takes
//! included — so the handler's own downstream write really happens; and from the instant
//! `DeleteEventSourceMapping` is accepted that mapping polls no more: a message sent afterwards
//! is neither handed to the function nor taken out of the queue, and the mapping is no longer
//! one a further call may act on (`UpdateEventSourceMapping` answers the modelled
//! `ResourceNotFoundException`, HTTP 404).**
//!
//! Four crates have to agree for that to hold, so the suite lives here: Amazon SQS holds both
//! queues and decides who may receive from them, IAM answers for the execution role the poller
//! reads the source as and the handler sends as, CloudFormation archives the inline handler
//! into the deployment package Lambda runs, and Lambda holds the function, the mapping and the
//! poller.
//!
//! The sibling suites pin the same chain elsewhere: `lambda_sqs_mapping_enabled_gate_on_a_started_emulator`
//! drives the `Enabled` gate on a started emulator with the handler writing to Amazon S3, and
//! `lambda_sqs_mapping_delete_stops_polling_on_an_authenticating_router` tables what the DELETE
//! leaves behind on a router assembled in the test process. What is reached only HERE is the
//! configuration a client gets when it states none of it: a function created with the DEFAULT
//! `Timeout` and queues created with their DEFAULT attributes, with the relay landing on a
//! second QUEUE that is drained by a LONG POLL, which is how a client watches for it.
//!
//! Every observation is one a client makes — `ReceiveMessage` and `GetQueueAttributes` on the
//! two queues, and Lambda's own calls — and every wait is on a CONDITION with a generous
//! deadline, so a loaded host delays a leg instead of weakening it. The absence leg looks again
//! over a quiet window rather than trusting one look: nothing should be polling then, so a
//! delivery the rule forbids has every chance to show up.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{deployed, output, Started};

/// The stack the execution role and the relay are deployed by, and the prefix the two queues
/// are named under.
const STACK: &str = "started-emulator-queue-relay";

/// The `State` a mapping polls its event source in, and the one a delete moves it into
/// (`EventSourceMappingConfiguration$State`).
const STATE_ENABLED: &str = "Enabled";
/// See [`STATE_ENABLED`].
const STATE_DELETING: &str = "Deleting";

/// The modelled error a mapping this account no longer holds for acting on answers with, and
/// the whole of the sentence that error class carries — it interpolates nothing of the request.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
/// See [`RESOURCE_NOT_FOUND`].
const NOT_FOUND_MESSAGE: &str = "The resource you requested does not exist.";

/// The `eventSource` every record of a queue-sourced event carries.
const EVENT_SOURCE: &str = "aws:sqs";

/// Where Lambda binds the mapping collection and one mapping of it.
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The attribute counting the messages a queue can hand over right now.
const VISIBLE_MESSAGES: &str = "ApproximateNumberOfMessages";

/// The environment variable the relay reads the sink queue's url from.
const SINK_VARIABLE: &str = "SINK_QUEUE_URL";

/// The body relayed while the mapping is held, and the one sent once it is gone.
const BEFORE: &str = "before-the-delete";
/// See [`BEFORE`].
const AFTER: &str = "after-the-delete";

/// How long each leg's condition is waited for, how long a drain's own long poll waits, and how
/// often this suite looks while waiting. Every wait is on a CONDITION; the spans are generous,
/// so a loaded host spends none of them.
const SETTLE_DEADLINE: Duration = Duration::from_secs(150);
/// See [`SETTLE_DEADLINE`].
const LONG_POLL_SECONDS: i64 = 5;
/// See [`SETTLE_DEADLINE`].
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long the absence leg keeps LOOKING once the mapping is gone: well past the span the
/// delivered leg took, so a delivery the rule forbids has every chance to appear.
const QUIET_WINDOW: Duration = Duration::from_secs(6);

/// The relay the stack deploys: it forwards the whole envelope it was handed to the queue its
/// environment names, with the SDK its managed runtime supplies. The client is built at module
/// scope, which is where a managed runtime's handler builds it.
const RELAY_HANDLER: &str = r#"import json
import os

import boto3

QUEUES = boto3.client("sqs")
SINK_URL = os.environ["SINK_QUEUE_URL"]


def handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    QUEUES.send_message(QueueUrl=SINK_URL, MessageBody=json.dumps(event))
    return {"relayed": [record["messageId"] for record in records]}
"#;

/// One Amazon SQS call, which must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.call(
        "sqs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &format!("AmazonSQS.{operation}")),
        ],
        body.to_string().into_bytes(),
    );
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon SQS {operation} must answer 200, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One Lambda `restJson1` call, as its status and parsed body.
fn lambda(emulator: &Started, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let (status, text) = emulator.call(
        "lambda",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    );
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A queue created with the DEFAULT attributes, as a client that states none gets it: its url
/// and the ARN it reports for itself.
fn given_queue(emulator: &Started, case: &str, name: &str) -> (String, String) {
    let created = sqs(emulator, case, "CreateQueue", &json!({ "QueueName": name }));
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue {name:?} reports no url: {created}"))
        .to_string();
    let arn = queue_attribute(emulator, case, &url, "QueueArn");
    (url, arn)
}

/// One attribute of the queue `url` addresses, as `GetQueueAttributes` reports it.
fn queue_attribute(emulator: &Started, case: &str, url: &str, name: &str) -> String {
    let read = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": [name] }),
    );
    read["Attributes"][name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue reports no {name}: {read}"))
        .to_string()
}

/// Every body the queue `url` addresses hands over to ONE long poll, left on the queue for the
/// next look: `VisibilityTimeout` 0, so a message this look saw is one the next look sees too
/// and nothing here consumes what the rule is about.
fn handed_over(emulator: &Started, case: &str, url: &str) -> Vec<String> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": LONG_POLL_SECONDS,
            "VisibilityTimeout": 0,
        }),
    );
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `Records[0].body` of a relayed envelope, or `None` for a body that is not one.
fn relayed_body(body: &str) -> Option<String> {
    let envelope: Value = serde_json::from_str(body).ok()?;
    let record = envelope["Records"].as_array()?.first()?;
    record["body"].as_str().map(str::to_string)
}

/// Look at the sink until it hands over an envelope whose first record carries `marker`, and
/// answer that envelope — which is how a client watches for a relay: a long poll, again and
/// again, until what it is waiting for is there.
fn relayed_envelope(emulator: &Started, case: &str, sink_url: &str, marker: &str) -> Value {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let bodies = handed_over(emulator, case, sink_url);
        let relayed = bodies
            .iter()
            .find(|body| relayed_body(body).as_deref() == Some(marker));
        if let Some(body) = relayed {
            return serde_json::from_str(body).unwrap_or(Value::Null);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the mapping must hand its function the message it polled and the \
             handler relays what it was handed, so the sink must come to hold an envelope \
             carrying {marker:?}; it handed over {bodies:?}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Wait until the mapping `uuid` names reports `state`, which the CLOCK owes.
fn await_state(emulator: &Started, case: &str, uuid: &str, state: &str) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = lambda(emulator, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None);
        if status == 200 && read["State"].as_str() == Some(state) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the mapping {uuid} never reported {state}, last read {status} \
             {read}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// The stack this suite deploys: the execution role Lambda reads the source queue as and the
/// handler sends as, and the relay running as it.
///
/// The grant names the three calls Lambda makes to consume a queue on the function's behalf —
/// the set AWS's own `AWSLambdaSQSQueueExecutionRole` grants, on THIS source alone — the log
/// writes an execution role carries, and the ONE send the handler itself makes, on the sink
/// alone. Nothing else: a delivery that needed a further grant would not be made here.
///
/// The function states no `Timeout` and no `MemorySize`, so it runs in the DEFAULTS a create
/// takes — which is the configuration a client that asks for neither gets.
fn wiring(source_arn: &str, sink_arn: &str, sink_url: &str) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "RelayRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "consume-the-source-and-relay",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "sqs:ReceiveMessage",
                                        "sqs:DeleteMessage",
                                        "sqs:GetQueueAttributes",
                                    ],
                                    "Resource": source_arn,
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": "sqs:SendMessage",
                                    "Resource": sink_arn,
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "logs:CreateLogGroup",
                                        "logs:CreateLogStream",
                                        "logs:PutLogEvents",
                                    ],
                                    "Resource": "*",
                                },
                            ],
                        },
                    }],
                },
            },
            "Relay": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["RelayRole", "Arn"] },
                    "Environment": { "Variables": { SINK_VARIABLE: sink_url } },
                    "Code": { "ZipFile": RELAY_HANDLER },
                },
            },
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "Relay" } } },
    })
}

/// RULE: on the emulator a client STARTS, a queue mapping reporting `Enabled` consumes its
/// source and runs the function it names — in the defaults a create that states none takes —
/// and from the delete onwards it consumes nothing and is no mapping a further call may act on.
#[test]
fn a_queue_mapping_relays_until_it_is_deleted_on_the_emulator_a_client_starts() {
    let case = "a relay wired by hand in the defaults a create takes";
    let emulator = Started::start();

    let (source_url, source_arn) = given_queue(&emulator, case, &format!("{STACK}-source"));
    let (sink_url, sink_arn) = given_queue(&emulator, case, &format!("{STACK}-sink"));

    let template = wiring(&source_arn, &sink_arn, &sink_url).to_string();
    let outputs = deployed(&emulator, case, STACK, &template);
    let function = output(&outputs, case, "FunctionName");

    let (status, created) = lambda(
        &emulator,
        "POST",
        MAPPINGS_URI,
        Some(&json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": 1,
        })),
    );
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping must answer 202, got {status} with {created}; \
         the log:\n{}",
        emulator.log()
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the created mapping reports no UUID: {created}"))
        .to_string();
    await_state(&emulator, case, &uuid, STATE_ENABLED);

    // The positive control: while the mapping is held it consumes, so the message reaches the
    // running handler and the envelope it was handed is relayed to the sink.
    sqs(
        &emulator,
        case,
        "SendMessage",
        &json!({ "QueueUrl": &source_url, "MessageBody": BEFORE }),
    );
    let envelope = relayed_envelope(&emulator, case, &sink_url, BEFORE);
    let records = envelope["Records"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: a mapping reading one message per batch hands over one record, got \
         {envelope}"
    );
    assert_eq!(
        (
            records[0]["body"].as_str(),
            records[0]["eventSource"].as_str(),
            records[0]["eventSourceARN"].as_str(),
        ),
        (Some(BEFORE), Some(EVENT_SOURCE), Some(source_arn.as_str())),
        "case {case:?}: the record must be about the message that was sent, on this source, got \
         {envelope}"
    );

    // The batch the invocation processed was deleted from the source: a message the mapping got
    // through is gone from the queue it came from.
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let visible = queue_attribute(&emulator, case, &source_url, VISIBLE_MESSAGES);
        if visible == "0" {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the message the invocation processed must be deleted from the \
             source, which reports {visible:?} still waiting; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }

    // The delete: the mapping answers the record in the state the teardown is in.
    let (status, deleted) = lambda(&emulator, "DELETE", &format!("{MAPPINGS_URI}/{uuid}"), None);
    assert_eq!(
        (status, deleted["State"].as_str()),
        (202, Some(STATE_DELETING)),
        "case {case:?}: the delete must answer the mapping as being deleted, got {status} with \
         {deleted}; the log:\n{}",
        emulator.log()
    );

    // A mapping the service no longer holds is no mapping a further call may act on.
    let (status, refused) = lambda(
        &emulator,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        Some(&json!({ "BatchSize": 2 })),
    );
    let code = refused["__type"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        (status, code.contains(RESOURCE_NOT_FOUND)),
        (404, true),
        "case {case:?}: UpdateEventSourceMapping on the deleted mapping must answer 404 \
         {RESOURCE_NOT_FOUND}, got {status} with {refused}"
    );
    assert_eq!(
        refused["Message"].as_str().or(refused["message"].as_str()),
        Some(NOT_FOUND_MESSAGE),
        "case {case:?}: that refusal carries the whole fixed sentence of its class, got {refused}"
    );

    // And nothing is consumed from here on: the message sent now is neither relayed nor taken
    // out of the source, however long this looks.
    sqs(
        &emulator,
        case,
        "SendMessage",
        &json!({ "QueueUrl": &source_url, "MessageBody": AFTER }),
    );
    let relayed_before: BTreeSet<String> = [BEFORE.to_string()].into_iter().collect();
    let quiet_until = Instant::now() + QUIET_WINDOW;
    loop {
        let relayed: BTreeSet<String> = handed_over(&emulator, case, &sink_url)
            .iter()
            .filter_map(|body| relayed_body(body))
            .collect();
        assert_eq!(
            relayed, relayed_before,
            "case {case:?}: a deleted mapping polls nothing, so the sink can hold no envelope \
             beyond the one relayed while it was held, got {relayed:?}; the log:\n{}",
            emulator.log()
        );
        if Instant::now() >= quiet_until {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }

    // The message nothing polled is still the source queue's to hand over, which is what makes
    // the absence a withholding rather than a loss.
    let waiting = handed_over(&emulator, case, &source_url);
    assert_eq!(
        waiting,
        vec![AFTER.to_string()],
        "case {case:?}: the message sent after the delete must still be receivable on the \
         source, got {waiting:?}"
    );
}
