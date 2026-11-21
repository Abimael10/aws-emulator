// SPDX-License-Identifier: CC0-1.0

//! The `UpdatePipe(DesiredState)` toggle of an EventBridge Pipes pipe between two Amazon SQS
//! queues, driven against the emulator a client STARTS: the built binary, on its own port,
//! with request authentication on, reached over TLS with SigV4-signed calls.
//!
//! RULE: **a pipe a client toggles through `UpdatePipe`'s own `DesiredState` follows it — set
//! `RUNNING` it receives from its Amazon SQS source, renders `TargetParameters.InputTemplate`
//! and sends the result to its target, deleting the record it consumed; set `STOPPED` it
//! consumes nothing, so a record sent afterwards reaches no target and stays waiting and
//! counted on the source; set `RUNNING` again it delivers that withheld record — and what the
//! target receives is the payload the producer sent, byte for byte, because a placeholder
//! selecting the record's payload as a whole carries that payload's own text.**
//!
//! Three crates have to agree for that to hold — EventBridge Pipes runs the pipe, Amazon SQS
//! holds both queues, IAM holds the role whose policies admit each stage's call — so the suite
//! lives here rather than in any one of them. The pipes crate's own
//! `tests/all/pipe_desired_state.rs` spans what the toggle generalises over (the batching
//! parameters the poll runs under, the size of the backlog a stop withholds) and
//! `tests/all/pipe_body_paths.rs` spans the payload spellings, both on a router a test
//! assembles in its own process. The emulator a client runs is a different configuration of
//! the same code: the credentials are verified, each call arrives over its own TLS connection,
//! and the pipe's own polling and its target delivery are made on the binary's own runtime
//! inside one process. This suite is the one that pins the chain THERE, so a toggle that moves
//! records in process and not on the started emulator is a failure a test reports rather than
//! one only a client sees. ONE toggle is driven here, because starting the binary costs what
//! starting the emulator costs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::element;

/// The prefix every name this suite creates carries.
const PREFIX: &str = "started-pipe-toggle";

/// The `PipeState` the pipe rests in under each `DesiredState`, and the `RequestedPipeState`
/// values `UpdatePipe` asks for (`RUNNING`, `STOPPED` — the two values of that enum).
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `TargetParameters.InputTemplate`: the one placeholder that selects the record's payload as
/// a whole, which is how a pipe forwards a message unchanged.
const BODY_TEMPLATE: &str = "<$.body>";

/// The payloads this suite sends, as the producer's own text: JSON documents spelled the way a
/// producer spells them (spacing of its own, a number a round trip would re-spell) rather than
/// the way a serializer would.
const CONTROL_PAYLOAD: &str = r#"{"marker": "control", "amount": 1.50}"#;
const WITHHELD_PAYLOAD: &str = "{\n  \"marker\": \"withheld\",\n  \"amount\": 2.00\n}";

/// How long the pipe's own settling and each delivery are given before this suite fails. Every
/// step is owed by the CLOCK, so each wait is on the CONDITION — looked for again until it
/// holds — and this bound is generous slack whose only job is that the wait TERMINATES.
/// Nothing below asserts on elapsed time.
const SETTLE: Duration = Duration::from_secs(120);

/// How long the target is watched for the record the STOP must withhold. A delivery that was
/// going to be made would have: it is many times the poller's own interval.
const QUIET: Duration = Duration::from_secs(3);

/// How often a pipe or a queue is looked at again while waiting.
const LOOK: Duration = Duration::from_millis(200);

/// One Amazon SQS call (`awsJson1_0`), as its status and parsed body.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
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
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// One queue of this suite: its url and the ARN the pipe names it by, read back through
/// `GetQueueAttributes` as a client reads it.
fn given_queue(emulator: &Started, name: &str) -> (String, String) {
    let (status, created) = sqs(emulator, "CreateQueue", &json!({ "QueueName": name }));
    assert_eq!(
        status, 200,
        "CreateQueue({name:?}) must answer 200, got {status} with {created}"
    );
    let url = created["QueueUrl"].as_str().unwrap_or_default().to_string();
    let (status, attributes) = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    );
    assert_eq!(
        status, 200,
        "GetQueueAttributes({name:?}) must answer 200, got {status} with {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!arn.is_empty(), "the queue {name:?} must report its ARN, got {attributes}");
    (url, arn)
}

/// The bodies waiting on the queue at `url` right now, deleted as they are taken, so a later
/// look sees only what arrived since.
fn drained(emulator: &Started, url: &str) -> Vec<String> {
    let (status, answered) = sqs(
        emulator,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    );
    assert_eq!(
        status, 200,
        "ReceiveMessage({url:?}) must answer 200, got {status} with {answered}"
    );
    let mut bodies = Vec::new();
    for message in answered["Messages"].as_array().cloned().unwrap_or_default() {
        bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
        let handle = message["ReceiptHandle"].as_str().unwrap_or_default();
        let (status, deleted) = sqs(
            emulator,
            "DeleteMessage",
            &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
        );
        assert_eq!(
            status, 200,
            "DeleteMessage({url:?}) must answer 200, got {status} with {deleted}"
        );
    }
    bodies
}

/// Everything that reached the queue at `url` until one body arrived, or until the deadline
/// passes.
fn until_delivered(emulator: &Started, url: &str) -> Vec<String> {
    let giving_up_at = Instant::now() + SETTLE;
    let mut seen = Vec::new();
    while seen.is_empty() && Instant::now() < giving_up_at {
        seen.extend(drained(emulator, url));
        if seen.is_empty() {
            std::thread::sleep(LOOK);
        }
    }
    seen
}

/// Everything that reached the queue at `url` over `window`.
fn arriving_during(emulator: &Started, url: &str, window: Duration) -> Vec<String> {
    let watching_until = Instant::now() + window;
    let mut seen = Vec::new();
    while Instant::now() < watching_until {
        seen.extend(drained(emulator, url));
        std::thread::sleep(LOOK);
    }
    seen
}

/// How many records the queue at `url` holds WAITING: the ones a consumer may still take.
fn waiting(emulator: &Started, url: &str) -> i64 {
    let (status, answered) = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["ApproximateNumberOfMessages"] }),
    );
    assert_eq!(
        status, 200,
        "GetQueueAttributes({url:?}) must answer 200, got {status} with {answered}"
    );
    answered["Attributes"]["ApproximateNumberOfMessages"]
        .as_str()
        .unwrap_or_default()
        .parse()
        .unwrap_or(-1)
}

/// One pipes call, as its status and parsed body.
fn pipes(emulator: &Started, method: &str, name: &str, body: Option<Value>) -> (u16, Value) {
    let payload = body
        .map(|body| body.to_string().into_bytes())
        .unwrap_or_default();
    let (status, text) = emulator.call(
        "pipes",
        method,
        &format!("/v1/pipes/{name}"),
        &[("content-type", "application/json")],
        payload,
    );
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The pipe `name` once it reports the `CurrentState` `want`, or whatever it reported last.
fn settled(emulator: &Started, name: &str, want: &str) -> Value {
    let giving_up_at = Instant::now() + SETTLE;
    loop {
        let (status, described) = pipes(emulator, "GET", name, None);
        assert_eq!(
            status, 200,
            "DescribePipe({name:?}) must answer 200, got {status} with {described}"
        );
        if described["CurrentState"].as_str() == Some(want) || Instant::now() >= giving_up_at {
            return described;
        }
        std::thread::sleep(LOOK);
    }
}

/// The trust document a role EventBridge Pipes acts as carries.
fn trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "pipes.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The execution role of the pipe below: every Amazon SQS call a queue-to-queue pipe makes,
/// allowed on exactly the resource it is made on.
fn given_role(emulator: &Started, source_arn: &str, target_arn: &str) -> String {
    let name = format!("{PREFIX}-role");
    let (status, created) = emulator.form_call(
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", &name),
            ("AssumeRolePolicyDocument", &trust()),
        ],
    );
    assert_eq!(
        status, 200,
        "CreateRole({name:?}) must answer 200, got {status} with {created}"
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes"],
                "Resource": [source_arn],
            },
            { "Effect": "Allow", "Action": ["sqs:SendMessage"], "Resource": [target_arn] },
        ],
    })
    .to_string();
    let (status, written) = emulator.form_call(
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", &name),
            ("PolicyName", "move-records"),
            ("PolicyDocument", &policy),
        ],
    );
    assert_eq!(
        status, 200,
        "PutRolePolicy({name:?}) must answer 200, got {status} with {written}"
    );
    element(&created, "Arn")
}

/// RULE: the pipe follows the `DesiredState` `UpdatePipe` records — it delivers while that
/// state rests `RUNNING`, withholds every record sent while it rests `STOPPED` (leaving them
/// waiting and counted on the source), and delivers that backlog once it rests `RUNNING`
/// again — and each delivery carries the payload its producer sent, byte for byte, on the
/// emulator a client starts.
#[test]
fn a_pipe_toggled_through_update_pipe_halts_and_resumes_its_delivery() {
    let emulator = Started::start();
    let (source_url, source_arn) = given_queue(&emulator, &format!("{PREFIX}-source"));
    let (target_url, target_arn) = given_queue(&emulator, &format!("{PREFIX}-target"));
    let role_arn = given_role(&emulator, &source_arn, &target_arn);

    let name = format!("{PREFIX}-pipe");
    let (status, created) = pipes(
        &emulator,
        "POST",
        &name,
        Some(json!({
            "RoleArn": role_arn,
            "Source": source_arn,
            "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
            "Target": target_arn,
            "TargetParameters": { "InputTemplate": BODY_TEMPLATE },
            "DesiredState": RUNNING,
        })),
    );
    assert_eq!(
        status, 200,
        "CreatePipe({name:?}) must answer 200, got {status} with {created}"
    );
    let described = settled(&emulator, &name, RUNNING);
    assert_eq!(
        described["CurrentState"].as_str(),
        Some(RUNNING),
        "the pipe must come to rest in {RUNNING}, got {described}; the emulator's log:\n{}",
        emulator.log()
    );

    // The control: while the pipe runs it moves a record, rendered by its input template.
    let (status, sent) = sqs(
        &emulator,
        "SendMessage",
        &json!({ "QueueUrl": source_url, "MessageBody": CONTROL_PAYLOAD }),
    );
    assert_eq!(
        status, 200,
        "SendMessage must answer 200, got {status} with {sent}"
    );
    let delivered = until_delivered(&emulator, &target_url);
    assert_eq!(
        delivered,
        vec![CONTROL_PAYLOAD.to_string()],
        "a running pipe must deliver the payload as sent, got {delivered:?}; the emulator's \
         log:\n{}",
        emulator.log()
    );

    // Stopped through UpdatePipe's own DesiredState, which is what a client that declares the
    // state rather than calling StopPipe sends.
    let (status, stopped) = pipes(
        &emulator,
        "PUT",
        &name,
        Some(json!({ "RoleArn": role_arn, "DesiredState": STOPPED })),
    );
    assert_eq!(
        status, 200,
        "UpdatePipe({name:?}) must answer 200, got {status} with {stopped}"
    );
    assert_eq!(
        stopped["DesiredState"].as_str(),
        Some(STOPPED),
        "UpdatePipe must echo the DesiredState it was asked for, got {stopped}"
    );
    let described = settled(&emulator, &name, STOPPED);
    assert_eq!(
        (
            described["CurrentState"].as_str(),
            described["DesiredState"].as_str()
        ),
        (Some(STOPPED), Some(STOPPED)),
        "the pipe must report both states {STOPPED} after the stop, got {described}"
    );

    // A record sent while it rests STOPPED: nothing is delivered and nothing is consumed, so
    // it is still waiting on the source.
    let (status, sent) = sqs(
        &emulator,
        "SendMessage",
        &json!({ "QueueUrl": source_url, "MessageBody": WITHHELD_PAYLOAD }),
    );
    assert_eq!(
        status, 200,
        "SendMessage must answer 200, got {status} with {sent}"
    );
    let quiet = arriving_during(&emulator, &target_url, QUIET);
    assert!(
        quiet.is_empty(),
        "a pipe stopped through UpdatePipe must deliver nothing, got {quiet:?}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    let held = waiting(&emulator, &source_url);
    assert_eq!(
        held, 1,
        "the withheld record must stay waiting and counted on the source, which reports \
         {held}"
    );

    // And back: the same member resumes the pipe, which delivers the backlog as sent.
    let (status, resumed) = pipes(
        &emulator,
        "PUT",
        &name,
        Some(json!({ "RoleArn": role_arn, "DesiredState": RUNNING })),
    );
    assert_eq!(
        status, 200,
        "UpdatePipe({name:?}) must answer 200, got {status} with {resumed}"
    );
    let described = settled(&emulator, &name, RUNNING);
    assert_eq!(
        described["CurrentState"].as_str(),
        Some(RUNNING),
        "the pipe must come to rest in {RUNNING} after the resume, got {described}"
    );
    let drained_backlog = until_delivered(&emulator, &target_url);
    assert_eq!(
        drained_backlog,
        vec![WITHHELD_PAYLOAD.to_string()],
        "the resumed pipe must deliver the withheld record as sent, got \
         {drained_backlog:?}; the emulator's log:\n{}",
        emulator.log()
    );

    // The record it delivered is consumed: the source is left holding nothing.
    let giving_up_at = Instant::now() + SETTLE;
    let mut left = waiting(&emulator, &source_url);
    while left != 0 && Instant::now() < giving_up_at {
        std::thread::sleep(LOOK);
        left = waiting(&emulator, &source_url);
    }
    assert_eq!(
        left, 0,
        "the drained backlog must be deleted from the source, which still reports {left}"
    );
}
