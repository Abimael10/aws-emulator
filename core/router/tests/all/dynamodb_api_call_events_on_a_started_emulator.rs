// SPDX-License-Identifier: CC0-1.0

//! Two properties of the management-event fan-out that only the emulator a customer STARTS
//! can show — the built binary, on its own port, with request authentication on, reached over
//! TLS with SigV4-signed calls.
//!
//! RULE: the event published for a served control-plane call is handed over as part of SERVING
//! that call, so a client whose `ReceiveMessage` long poll is ALREADY IN FLIGHT when the call
//! is made is handed the envelope by that same poll; and a rule pinned to another call's name
//! is left HOLDING NOTHING by that publication — its queue reports zero messages, visible and
//! in flight, rather than merely handing none to a read.
//!
//! The sibling suite `all/cloudtrail_api_call_events_on_a_started_emulator` pins the channel
//! itself on the started emulator — the control write, the subject create and the near-miss
//! delete, each read back after the call — and
//! `all/cloudtrail_dynamodb_api_call_events` spans the envelope fields a pattern may pin, in
//! process. Neither drives a poll that is already waiting when the call falls due, and neither
//! reads a near-miss queue's own depth: those two are this suite's subject. They need the
//! started binary, because what they are about is a delivery made on its own multi-thread
//! runtime while a client is blocked on a socket of its own.
//!
//! ONE start is paid for here, since starting the binary costs what starting the emulator
//! costs, and it drives both properties from one publication. Every wait is on a CONDITION,
//! and every assertion quotes the emulator's own log, which is where a delivery it could not
//! make says so.

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

/// The Region the started emulator's published configuration names, which is the Region
/// [`Started`] signs and addresses every call for.
const REGION: &str = "us-east-1";

/// The account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The `detail-type` CloudTrail's management-event fan-out carries, and the `source` and
/// `eventSource` a DynamoDB call's event and record carry. Spelled out here rather than
/// imported from a service crate, so a wrong constant in the implementation cannot make this
/// suite pass.
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";
const DYNAMODB_EVENT_SOURCE: &str = "aws.dynamodb";
const DYNAMODB_RECORD_SOURCE: &str = "dynamodb.amazonaws.com";

/// The service principal EventBridge delivers to an SQS target as, which is what the queue's
/// resource policy admits.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// How long the client's own long poll waits for a message to arrive, in seconds — "The
/// duration (in seconds) for which the call waits for a message to arrive in the queue before
/// returning" (`ReceiveMessageRequest$WaitTimeSeconds`). A long poll is how a client waits for
/// a delivery it cannot otherwise be told about, and it is the wait this suite is about.
const WAIT_TIME_SECONDS: u64 = 20;

/// The table the subject call creates.
const TABLE: &str = "started-in-flight-api-call-table";

/// One RPC call over the started emulator's TLS endpoint, which must answer 200, as the JSON
/// it answered.
fn rpc(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> Value {
    let (status, answered) = emulator.rpc(
        signing_name,
        target,
        &format!("application/x-amz-json-{json_version}"),
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: {target} must be served, got HTTP {status} with {answered}"
    );
    serde_json::from_str(&answered)
        .unwrap_or_else(|error| panic!("case {case:?}: {answered} must be JSON: {error}"))
}

/// One EventBridge call (`awsJson1_1`).
fn events(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "events",
        &format!("AWSEvents.{operation}"),
        "1.1",
        body,
    )
}

/// One Amazon SQS call (`awsJson1_0`).
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "1.0",
        body,
    )
}

/// One CloudTrail call (`awsJson1_1`).
fn cloudtrail(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "cloudtrail",
        &format!("CloudTrail_20131101.{operation}"),
        "1.1",
        body,
    )
}

/// A string member of a JSON answer, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The bucket a trail delivers its log files into, carrying the policy that lets CloudTrail
/// write into it — created the way a client creates it, because CloudTrail resolves the
/// destination in the account's object store before it accepts the trail.
fn trail_bucket(emulator: &Started, bucket: &str) {
    let (status, body) = emulator.s3("PUT", &format!("/{bucket}"), "");
    assert_eq!(
        status, 200,
        "setup: CreateBucket({bucket:?}) answered {status}: {body}"
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AWSCloudTrailWrite",
            "Effect": "Allow",
            "Principal": { "Service": "cloudtrail.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
            "Condition": { "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" } },
        }],
    })
    .to_string();
    let (status, body) = emulator.s3("PUT", &format!("/{bucket}?policy"), &policy);
    assert!(
        (200..300).contains(&status),
        "setup: PutBucketPolicy({bucket:?}) answered {status}: {body}"
    );
}

/// The rule `slug` on the default bus, pinned to one control-plane call of one service, with
/// its own queue admitting EventBridge's send on behalf of that rule and installed as its
/// target; answers the queue's url.
fn wired(emulator: &Started, slug: &str, event_name: &str) -> String {
    let created = sqs(emulator, slug, "CreateQueue", &json!({ "QueueName": slug }));
    let url = member(slug, &created, "/QueueUrl");
    // The queue's ARN as the service that owns it reports it, rather than one this suite
    // spells for itself.
    let attributes = sqs(
        emulator,
        slug,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = member(slug, &attributes, "/Attributes/QueueArn");
    assert_eq!(
        queue_arn,
        format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{slug}"),
        "case {slug:?}: the queue's ARN is the one a client writes its grant over"
    );
    let pattern = json!({
        "source": [DYNAMODB_EVENT_SOURCE],
        "detail-type": [API_CALL_DETAIL_TYPE],
        "detail": {
            "eventSource": [DYNAMODB_RECORD_SOURCE],
            "eventName": [event_name],
        },
    });
    let put = events(
        emulator,
        slug,
        "PutRule",
        &json!({ "Name": slug, "EventPattern": pattern.to_string() }),
    );
    let rule_arn = member(slug, &put, "/RuleArn");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        emulator,
        slug,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": document.to_string() } }),
    );
    let targeted = events(
        emulator,
        slug,
        "PutTargets",
        &json!({
            "Rule": slug,
            "Targets": [{ "Id": "the-queue", "Arn": &queue_arn }],
        }),
    );
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {slug:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// RULE: the envelope reaches a long poll that was already in flight when the call was made,
/// and the rule pinned to another call's name is left holding nothing by that publication.
#[test]
fn a_fan_out_reaches_a_poll_already_in_flight_and_leaves_a_near_miss_empty() {
    let emulator = Started::start();

    // A trail of the account, logging: the condition the fan-out is made under.
    trail_bucket(&emulator, "started-in-flight-api-call-trail");
    cloudtrail(
        &emulator,
        "setup",
        "CreateTrail",
        &json!({
            "Name": "started-in-flight-api-calls",
            "S3BucketName": "started-in-flight-api-call-trail",
        }),
    );
    cloudtrail(
        &emulator,
        "setup",
        "StartLogging",
        &json!({ "Name": "started-in-flight-api-calls" }),
    );
    let status = cloudtrail(
        &emulator,
        "setup",
        "GetTrailStatus",
        &json!({ "Name": "started-in-flight-api-calls" }),
    );
    assert_eq!(
        status["IsLogging"],
        json!(true),
        "setup: the trail must be logging, got {status}; the emulator's log:\n{}",
        emulator.log()
    );

    // The rule the call's event is matched by, and the rule pinned to another call of the same
    // service, each on its own queue so what arrived at one is never read as the other's.
    let subject = wired(&emulator, "started-in-flight-subject", "CreateTable");
    let near_miss = wired(&emulator, "started-in-flight-near-miss", "DeleteTable");

    // The client's long poll is started FIRST and the call is made while it waits.
    let received = std::thread::scope(|scope| {
        let polling = scope.spawn(|| {
            sqs(
                &emulator,
                "the in-flight poll",
                "ReceiveMessage",
                &json!({
                    "QueueUrl": &subject,
                    "MaxNumberOfMessages": 10,
                    "WaitTimeSeconds": WAIT_TIME_SECONDS,
                }),
            )
        });
        let answered = rpc(
            &emulator,
            "the call made while the poll waits",
            "dynamodb",
            "DynamoDB_20120810.CreateTable",
            "1.0",
            &json!({
                "TableName": TABLE,
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        );
        // The call is served while the client is blocked on its own socket.
        assert_eq!(
            member(
                "the call made while the poll waits",
                &answered,
                "/TableDescription/TableName"
            ),
            TABLE,
            "the call must be answered with the table it created, got {answered}"
        );
        polling.join().expect("the polling thread")
    });

    // The poll that was ALREADY in flight is the one handed the envelope: a delivery made only
    // after it had ended would leave it empty, whatever arrived later.
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        messages.len(),
        1,
        "the in-flight poll must be handed the published envelope, got {received}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    let body = messages[0]["Body"].as_str().unwrap_or_default();
    let published: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("a delivered envelope is JSON ({error}): {body}"));
    for (pointer, expected) in [
        ("/source", json!(DYNAMODB_EVENT_SOURCE)),
        ("/detail-type", json!(API_CALL_DETAIL_TYPE)),
        ("/account", json!(ACCOUNT)),
        ("/region", json!(REGION)),
        ("/detail/eventName", json!("CreateTable")),
        ("/detail/eventSource", json!(DYNAMODB_RECORD_SOURCE)),
        ("/detail/readOnly", json!(false)),
        ("/detail/requestParameters/tableName", json!(TABLE)),
    ] {
        let actual = published.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "expected {pointer} to be {expected}, got {actual} in {published}"
        );
    }

    // One publication is matched against every rule of the bus, so once the subject's own poll
    // has been handed it the near-miss rule has been decided against that same event: its
    // queue HOLDING nothing — nothing visible and nothing in flight — is a verdict rather than
    // a race.
    let reported = sqs(
        &emulator,
        "the near miss",
        "GetQueueAttributes",
        &json!({
            "QueueUrl": &near_miss,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    );
    for attribute in [
        "ApproximateNumberOfMessages",
        "ApproximateNumberOfMessagesNotVisible",
    ] {
        assert_eq!(
            reported["Attributes"][attribute],
            json!("0"),
            "the near-miss queue must report {attribute} \"0\", got {reported}"
        );
    }
    let read = sqs(
        &emulator,
        "the near miss",
        "ReceiveMessage",
        &json!({ "QueueUrl": &near_miss, "MaxNumberOfMessages": 10 }),
    );
    assert!(
        read["Messages"].as_array().is_none_or(Vec::is_empty),
        "the near-miss queue must hand over no message, got {read}"
    );
}
