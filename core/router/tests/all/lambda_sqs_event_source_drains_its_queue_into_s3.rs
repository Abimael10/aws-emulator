// SPDX-License-Identifier: CC0-1.0

//! The whole SQS -> Lambda consumer chain, across the services that own its halves: Amazon SQS
//! holds the queue and the messages on it, IAM answers for the execution role the mapping's
//! poller reads as, CloudFormation archives the inline handler into the function's deployment
//! package, Lambda POLLS the queue and runs that handler, and Amazon S3 holds the objects the
//! handler itself writes.
//!
//! The rule pinned here: **an event source mapping that reports `Enabled` over a queue reads
//! that queue on its own, so every message that becomes visible on it is handed to the mapped
//! function as a record of an invocation's `Records` — whatever put the message there — and the
//! invocation's own downstream effects really happen; a batch the invocation processed is
//! deleted from the queue, and a function that declares `ReportBatchItemFailures` leaves
//! exactly the `itemIdentifiers` its response named for redelivery once the queue's
//! `VisibilityTimeout` lapses, while the rest of the batch is not handed over again.**
//!
//! Five crates have to agree for that to hold, so the suite lives here rather than in any one
//! of them: a crate-local suite can see the poll or the invocation, never the effect the
//! handler had in a third service. Every observation is one a client makes — `ListObjectsV2`
//! and `GetObject` on the bucket the handler writes, `ReceiveMessage` on the source queue, and
//! Lambda's own `GetEventSourceMapping`.
//!
//! The handler writes ONE object per record it is handed, keyed by that record's `messageId`
//! and the delivery count the queue reports for it, so a client can tell apart "the function
//! was never invoked" (no object), "it was invoked once" (one object) and "the message was
//! handed over again" (a second object for the same message under a higher count).
//!
//! The rows of the first test differ along what the rule generalises over — how many messages
//! are waiting, how large a batch the mapping asks for, whether the messages carry message
//! attributes, and whether the body is a document at all. The second test changes the PRODUCER:
//! the message is put on the queue by Amazon S3's own object-created notification rather than
//! by a client, which is the same rule over a message the client never sent. The third changes
//! the function's RESPONSE: a partial-batch report, where only the reported record comes back.
//!
//! The sibling [`crate::lambda_sqs_event_source_failed_batch_keeps_its_message`] pins what a
//! WHOLLY failed invocation leaves behind, and the owning crate's own
//! `sqs_event_source_partial_batch_redrive` tables the response shapes a partial report may
//! take; what is reached only here is the handler's effect in a third service and the producer
//! being another service.
//!
//! The wire harnesses are the ones that already exist: the Amazon SQS and AWS Lambda calls, the
//! queue and the wait for a mapping to settle come from
//! [`crate::lambda_sqs_event_source_batching_window`], and the S3 request harness, the sized
//! thread and the stack reader from
//! [`crate::s3_notification_invokes_its_lambda_target`] and [`wait_for_stack_status`], so the
//! way a client reaches this emulator is described in one place.
//!
//! Every request is dispatched through a boxed future on a thread sized like the emulator's own
//! boot path ([`driven`]): the chain this drives — S3's publication into Amazon SQS, Lambda's
//! poll of that queue, and the handler's own SDK call back into S3 — nests more future than a
//! default test thread's stack holds.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::lambda_sqs_event_source_batching_window::{
    await_state, given_queue_hiding_for, lambda, queue_attribute, sqs as sqs_call,
    IN_FLIGHT_MESSAGES, MAPPINGS_URI, STATE_ENABLED,
};
use crate::query_wire::{elements, outputs_of};
use crate::s3_notification_invokes_its_lambda_target::{
    build_router, create_bucket, driven, query, s3,
};

/// The `State` a function serves invocations in ("the function is ready to be invoked",
/// `FunctionConfiguration$State`).
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` each consumer stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long each queue hides a message it hands a consumer, in seconds, and the mapped
/// function's own `Timeout`: Lambda takes a queue mapping only while the queue's
/// `VisibilityTimeout` is at least the function's `Timeout`, and this is the span after which a
/// message no invocation processed is visible again.
const HOLD_SECONDS: i64 = 3;

/// The key prefix the handler writes every record it was handed under.
const RECORD_PREFIX: &str = "records/";

/// The `eventSource` every record of a queue-sourced event carries.
const SQS_EVENT_SOURCE: &str = "aws:sqs";

/// The `FunctionResponseType` that makes Lambda read a partial-batch report out of the
/// invocation's response (`smithy_query(query="shape",shape="FunctionResponseType")`).
const REPORT_BATCH_ITEM_FAILURES: &str = "ReportBatchItemFailures";

/// How long an observation of work the CLOCK owes — a stack coming up, a function going
/// `Active`, a mapping settling, an invocation, a redelivery — keeps looking before giving up,
/// and how long between two looks. The deadline of a CONDITION, not a measurement of it: well
/// above what the work takes even on a host running every suite of this workspace at once, so a
/// row fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(150);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a check that something must NOT happen keeps looking, once the queue reports
/// nothing waiting and nothing in flight and the mapping that polled it is gone: a redelivery
/// the rule forbids has every chance to show up, because nothing is holding a message and
/// nothing is polling for one.
const QUIET_WINDOW: Duration = Duration::from_secs(2);

/// The attribute that says how many of a queue's messages a consumer may be handed right now:
/// "ApproximateNumberOfMessages" (`GetQueueAttributes$AttributeNames`). Its in-flight
/// counterpart is the sibling suite's [`IN_FLIGHT_MESSAGES`].
const VISIBLE_MESSAGES: &str = "ApproximateNumberOfMessages";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The event class the notification configuration below is recorded for.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The service principal Amazon S3 publishes a bucket's notifications under — "for Amazon Web
/// Services services, the principal is a domain-style identifier that the service defines, such
/// as s3.amazonaws.com" (`AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// The action Amazon S3 performs to publish a notification to a queue destination, and the
/// condition key a grant narrowed to ONE publishing bucket names it by.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The `Event` of the test notification a validated queue destination receives when the
/// configuration is installed, drained as a client before the mapping is created.
const TEST_EVENT: &str = "s3:TestEvent";

/// The `FAIL_ON` value of a row whose handler must take every record: no body below equals it,
/// so the response reports no failure.
const NOTHING_FAILS: &str = "no body sent by this suite equals this";

/// The handler every consumer below deploys inline: for each record of the event it writes ONE
/// object naming that record's `messageId` and the delivery count the queue reports for it,
/// carrying what the record said, and then reports as failed exactly the records whose body is
/// the one its environment names.
///
/// Writing per `messageId` AND delivery count is what makes a redelivery observable: a message
/// handed over twice leaves two objects, a message processed once leaves one.
const RECORDING_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    client = boto3.client("s3")
    bucket = os.environ["MARKER_BUCKET"]
    fail_on = os.environ["FAIL_ON"]
    failures = []
    for record in event["Records"]:
        attempt = record["attributes"]["ApproximateReceiveCount"]
        marker = {
            "messageId": record["messageId"],
            "body": record["body"],
            "eventSource": record["eventSource"],
            "eventSourceARN": record["eventSourceARN"],
            "messageAttributes": record["messageAttributes"],
            "attempt": attempt,
        }
        client.put_object(
            Bucket=bucket,
            Key="records/" + record["messageId"] + "-" + attempt,
            Body=json.dumps(marker).encode("utf-8"),
        )
        if record["body"] == fail_on:
            failures.append({"itemIdentifier": record["messageId"]})
    return {"batchItemFailures": failures}
"#;

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
///
/// The wire form itself is the sibling suite's
/// ([`crate::lambda_sqs_event_source_batching_window`]), which every router-level suite over a
/// queue mapping reaches for; what is added here is that the call SUCCEEDED, since no step of
/// this chain is a refusal.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = sqs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name` hiding a delivered message for [`HOLD_SECONDS`]; answers its url and
/// the ARN Amazon SQS itself reports for it.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    given_queue_hiding_for(router, case, name, HOLD_SECONDS).await
}

/// The stack one consumer is deployed as: the execution role the poller reads and the handler
/// writes as, and the function whose inline code CloudFormation archives into its deployment
/// package.
fn consumer_stack(marker_bucket: &str, fail_on: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ConsumerRole": {
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
                    // Lambda reads the event source as this role, and the handler writes its
                    // objects as it, so the grant names the reads the poller makes and the
                    // write the handler makes.
                    "Policies": [{
                        "PolicyName": "poll-the-queue-and-write-the-record",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "sqs:ReceiveMessage",
                                    "sqs:DeleteMessage",
                                    "sqs:GetQueueAttributes",
                                    "s3:PutObject",
                                    "s3:GetObject",
                                    "logs:*",
                                ],
                                // The queues are created per row, after the stack, so the grant
                                // names the actions rather than each row's own queue.
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["ConsumerRole", "Arn"] },
                    "Timeout": HOLD_SECONDS,
                    "Environment": {
                        "Variables": { "MARKER_BUCKET": marker_bucket, "FAIL_ON": fail_on },
                    },
                    "Code": { "ZipFile": RECORDING_HANDLER },
                },
            },
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "Consumer" } } },
    })
    .to_string()
}

/// Deploy `stack` from `template`, wait for it to come up, and answer the name Lambda holds the
/// function under once it reports the function `Active`.
async fn deployed_consumer(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the consumer's stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let function = outputs_of(&described)
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack publishes no FunctionName, got {described}"));
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        if status == 200 && configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return function;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {function} {FUNCTION_ACTIVE}, last read \
             {status} {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The mapping of `function` onto `queue_arn`, asking for `batch_size` records per batch and for
/// `response_types` as its `FunctionResponseTypes`; answers its `UUID` once Lambda reports it
/// `Enabled`, which is the state a mapping polls its source in.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    queue_arn: &str,
    batch_size: i64,
    response_types: &[&str],
) -> String {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": queue_arn,
        "BatchSize": batch_size,
        "Enabled": true,
    });
    if !response_types.is_empty() {
        request["FunctionResponseTypes"] = json!(response_types);
    }
    let (status, created) = lambda(router, "POST", MAPPINGS_URI, Some(&request)).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {status} with \
         {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the created mapping reports no UUID, got {created}"))
        .to_string();
    // A mapping polls its source in `Enabled`, which the CLOCK owes after the create; the wait
    // for it is the sibling suite's.
    await_state(router, case, &uuid, STATE_ENABLED).await;
    uuid
}

/// Stop the mapping `uuid` from polling, so what the queue holds afterwards is the client's to
/// read.
async fn delete_mapping(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let (status, deleted) = lambda(router, "DELETE", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from DeleteEventSourceMapping, got {status} with \
         {deleted}"
    );
}

/// Every record the handler has written into `bucket`, keyed by the object key it wrote it
/// under (`<messageId>-<deliveryCount>`).
async fn written_records(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
) -> BTreeMap<String, Value> {
    let listed = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={RECORD_PREFIX}"),
        "",
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListObjectsV2({bucket:?}) must succeed, got {} with {}",
        listed.status, listed.body
    );
    let mut found = BTreeMap::new();
    for key in elements(&listed.body, "Key") {
        let object = s3(router, "GET", &format!("/{bucket}/{key}"), "").await;
        assert_eq!(
            object.status, 200,
            "case {case:?}: GetObject({bucket:?}, {key:?}) must succeed, got {} with {}",
            object.status, object.body
        );
        let parsed: Value = serde_json::from_str(&object.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the record the handler wrote must be a JSON document ({error}), \
                 got {}",
                object.body
            )
        });
        found.insert(key, parsed);
    }
    found
}

/// Look at `bucket` until the handler has written at least `wanted` records, and answer what it
/// holds — bounded by [`BUDGET`], which is the condition's deadline rather than a measurement.
async fn awaited_records(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    wanted: usize,
) -> BTreeMap<String, Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let found = written_records(router, case, bucket).await;
        if found.len() >= wanted {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the mapping to hand its function {wanted} record(s), it \
             wrote {:?}",
            found.keys().collect::<Vec<&String>>()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Take every message waiting on the queue at `url` off it, as the body each carries.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let mut found = Vec::new();
    loop {
        let answered = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = answered["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return found;
        }
        for message in messages {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            found.push(message["Body"].as_str().unwrap_or_default().to_string());
        }
    }
}

/// Wait until the queue at `url` holds NOTHING — neither a message a consumer may be handed
/// nor one in flight with one — and then keep looking for [`QUIET_WINDOW`], answering every
/// body that came back in that time.
///
/// The deletion of a batch an invocation processed is what a client sees as an empty queue, so
/// the wait is on that CONDITION: a pass that is still working on its batch holds the messages
/// in flight, which is why the in-flight count is waited on too rather than the visible one
/// alone. Once both are zero nothing is holding anything, so the window that follows says that
/// nothing came BACK — with the mapping already deleted, nothing is polling to put it there.
async fn awaited_empty_queue(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let visible = queue_attribute(router, case, url, VISIBLE_MESSAGES).await;
        let in_flight = queue_attribute(router, case, url, IN_FLIGHT_MESSAGES).await;
        if visible.as_deref() == Some("0") && in_flight.as_deref() == Some("0") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every message the invocation processed must be deleted from the \
             queue, {url} still reports {visible:?} waiting and {in_flight:?} in flight"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let until = Instant::now() + QUIET_WINDOW;
    let mut seen = Vec::new();
    while Instant::now() < until {
        seen.extend(drained(router, case, url).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    seen
}

/// Wait until no message of the queue at `url` is in flight with a consumer, so no pass of a
/// mapping is still working on a batch of it and what the queue holds is settled.
async fn awaited_nothing_in_flight(router: &MultiServiceRouter, case: &str, url: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let in_flight = queue_attribute(router, case, url, IN_FLIGHT_MESSAGES).await;
        if in_flight.as_deref() == Some("0") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no batch of {url} may still be in flight once the mapping is gone, \
             it reports {in_flight:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the first table: what is waiting on the row's queue, and what the mapping asks
/// for.
struct Row {
    case: &'static str,
    /// The slug the row's resources are named from.
    slug: &'static str,
    /// The bodies the row sends, one message each.
    bodies: &'static [&'static str],
    /// `BatchSize` the row's mapping asks for.
    batch_size: i64,
    /// What `SendMessage` is given as `MessageAttributes`, keyed by attribute name — empty for a
    /// row that sends none.
    sent: fn() -> Value,
    /// What the record the function is handed must carry under `messageAttributes`: the
    /// `dataType` the sender gave beside that attribute's own value.
    expected: fn() -> Value,
}

/// The rows: one per shape of what is waiting to be polled.
///
/// They differ along what the rule generalises over — how many messages are waiting, the
/// `BatchSize` the mapping asks for, whether the messages carry attributes, and whether a body
/// is a document at all. `String` and `Number` are two of the data types Amazon SQS publishes
/// for `MessageAttributeValue$DataType` ("Amazon SQS supports the following logical data types:
/// String, Number, and Binary", `smithy_query(query="doc",shape="MessageAttributeValue")`); the
/// whole catalog is tabled in the owning crate's own suite, since it needs no third service.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one message carrying a String attribute beside a Number one, one per batch",
            slug: "esm-into-s3-typed",
            bodies: &["{\"order\": \"A-1\"}"],
            batch_size: 1,
            sent: || {
                json!({
                    "orderId": { "DataType": "String", "StringValue": "A-1" },
                    "attempts": { "DataType": "Number", "StringValue": "7" },
                })
            },
            expected: || {
                json!({
                    "orderId": { "dataType": "String", "stringValue": "A-1" },
                    "attempts": { "dataType": "Number", "stringValue": "7" },
                })
            },
        },
        Row {
            case: "three messages carrying no attributes, a batch of up to ten",
            slug: "esm-into-s3-three",
            bodies: &["m-1", "m-2", "m-3"],
            batch_size: 10,
            sent: || json!({}),
            expected: || json!({}),
        },
        Row {
            case: "a body that is not a document at all, a batch of up to two",
            slug: "esm-into-s3-opaque",
            bodies: &["a body that is not a JSON document"],
            batch_size: 2,
            sent: || json!({}),
            expected: || json!({}),
        },
    ]
}

/// `PUT /<bucket>?notification` recording ONE queue configuration for object-created events.
///
/// The elements are the ones S3's own model binds the queue destination list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`: `QueueConfigurations` is
/// bound as `QueueConfiguration`, whose ARN member is bound as `Queue`).
async fn put_queue_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    queue_arn: &str,
) {
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><QueueConfiguration>\
         <Id>into-the-mapped-queue</Id><Queue>{queue_arn}</Queue>\
         <Event>{OBJECT_CREATED}</Event></QueueConfiguration></NotificationConfiguration>"
    );
    let answer = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({bucket:?}) must record the queue \
         destination, got {} with {}",
        answer.status, answer.body
    );
}

/// The queue policy letting Amazon S3 publish the notifications of `bucket` to `queue_arn`,
/// narrowed by `aws:SourceArn` — the key such a grant names ONE publishing bucket by. A bucket's
/// ARN carries no Region and no account, because a bucket name is globally unique.
fn notification_grant(queue_arn: &str, bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowS3Notifications",
            "Effect": "Allow",
            "Principal": { "Service": S3_PRINCIPAL },
            "Action": SEND_MESSAGE_ACTION,
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { SOURCE_ARN_KEY: format!("arn:aws:s3:::{bucket}") } },
        }],
    })
    .to_string()
}

/// Drain the queue at `url` until the [`TEST_EVENT`] probe a validated queue destination
/// receives on install has been taken off it: that probe is what says the configuration was
/// installed against a queue this suite reads, and taking it off lets the object write below be
/// observed on its own.
async fn awaited_test_event(router: &MultiServiceRouter, case: &str, url: &str) {
    let deadline = Instant::now() + BUDGET;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        if seen.iter().any(|body| body.contains(TEST_EVENT)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: installing the configuration must send the queue the {TEST_EVENT} \
             probe, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The records of `written` that belong to `message_id`, as the delivery counts they were
/// written under, in ascending order.
///
/// `ApproximateReceiveCount` is a decimal count the queue reports as text, so it is read as the
/// number it is: ordering the texts would put a two-digit count before a one-digit one. A count
/// that is not a number at all reads as 0, which fails the assertion that expects a delivery.
fn attempts_of(written: &BTreeMap<String, Value>, message_id: &str) -> Vec<u64> {
    let mut attempts: Vec<u64> = written
        .values()
        .filter(|record| record["messageId"].as_str() == Some(message_id))
        .map(|record| {
            record["attempt"]
                .as_str()
                .and_then(|count| count.parse().ok())
                .unwrap_or_default()
        })
        .collect();
    attempts.sort_unstable();
    attempts
}

/// The assertions one record owes the attributes its message was sent with: every name the
/// sender set is there, each carrying that attribute's own `dataType` and value, and no name the
/// sender did not set is.
///
/// Stated per NAME rather than as one comparison of the whole map, because the rule is about the
/// attributes the sender set: a record may carry more about each of them (the empty
/// `stringListValues` and `binaryListValues` a queue-source record has beside the value) without
/// the rule being broken.
fn assert_attributes(case: &str, record: &Value, expected: &Value) {
    let expected = expected
        .as_object()
        .expect("the row's expected attributes are a map");
    let carried = record["messageAttributes"]
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: the record carries no messageAttributes, got {record}"));
    for (name, wanted) in expected {
        let held = carried
            .get(name)
            .unwrap_or_else(|| panic!("case {case:?}: the record must carry {name:?}, got {record}"));
        for (member, value) in wanted.as_object().expect("an attribute is a map") {
            assert_eq!(
                held.get(member),
                Some(value),
                "case {case:?}: expected {name:?} to carry {member} {value}, got {held}"
            );
        }
    }
    let unexpected: Vec<&String> = carried
        .keys()
        .filter(|name| !expected.contains_key(name.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "case {case:?}: the record must carry only the attributes the sender set, it also carried \
         {unexpected:?}"
    );
}

/// RULE, for a message a client sends: an `Enabled` mapping over a queue polls it on its own, so
/// every message waiting on it is handed to the mapped function as a record carrying the
/// message's id, body and attributes, the invocation's own effect in a third service really
/// happens, and the batch it processed is deleted from the queue.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_mapped_queue_hands_every_message_to_its_function_whose_effect_reaches_s3() {
    driven("lambda-sqs-esm-drains-into-s3", || {
        Box::pin(a_mapped_queue_hands_every_message_to_its_function_whose_effect_reaches_s3_body())
    });
}

async fn a_mapped_queue_hands_every_message_to_its_function_whose_effect_reaches_s3_body() {
    let router = build_router();
    let setup = "the shared wiring";
    let markers = "esm-into-s3-markers";
    create_bucket(&router, setup, markers).await;
    let function = deployed_consumer(
        &router,
        setup,
        "esm-into-s3-consumer",
        &consumer_stack(markers, NOTHING_FAILS),
    )
    .await;

    for row in rows() {
        let case = row.case;
        let (url, arn) = given_queue(&router, case, &format!("{}-source", row.slug)).await;
        let uuid = given_mapping(&router, case, &function, &arn, row.batch_size, &[]).await;

        // Everything the row sends is on the queue before anything is read, so the whole of what
        // was waiting is what the mapping had to hand over.
        let mut sent_ids: Vec<(String, String)> = Vec::new();
        for body in row.bodies {
            let mut request = json!({ "QueueUrl": url, "MessageBody": body });
            let attributes = (row.sent)();
            if attributes.as_object().is_some_and(|sent| !sent.is_empty()) {
                request["MessageAttributes"] = attributes;
            }
            let sent = sqs(&router, case, "SendMessage", &request).await;
            let id = sent["MessageId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: SendMessage reports no MessageId, got {sent}"))
                .to_string();
            sent_ids.push((id, (*body).to_string()));
        }

        let written = awaited_records(&router, case, markers, row.bodies.len()).await;
        for (id, body) in &sent_ids {
            let record = written
                .values()
                .find(|record| record["messageId"].as_str() == Some(id.as_str()))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the mapping must hand its function the message {id}, it \
                         wrote {:?}",
                        written.keys().collect::<Vec<&String>>()
                    )
                });
            assert_eq!(
                record["body"].as_str(),
                Some(body.as_str()),
                "case {case:?}: the record must carry the body as the queue holds it, got {record}"
            );
            assert_eq!(
                record["eventSource"].as_str(),
                Some(SQS_EVENT_SOURCE),
                "case {case:?}: the record must name its event source, got {record}"
            );
            assert_eq!(
                record["eventSourceARN"].as_str(),
                Some(arn.as_str()),
                "case {case:?}: the record must name the queue it came from, got {record}"
            );
            assert_attributes(case, record, &(row.expected)());
            assert_eq!(
                attempts_of(&written, id),
                vec![1],
                "case {case:?}: a message one invocation processed is handed over once, got the \
                 deliveries {:?} for {id}",
                attempts_of(&written, id)
            );
        }

        // The batch the invocation processed is DELETED: with the mapping gone nothing polls the
        // queue, and the queue itself reports nothing waiting and nothing in flight, after which
        // nothing comes back.
        delete_mapping(&router, case, &uuid).await;
        let back = awaited_empty_queue(&router, case, &url).await;
        assert!(
            back.is_empty(),
            "case {case:?}: every message the invocation processed must be deleted from the \
             queue, {url} handed back {back:?}"
        );

        // The marker bucket is shared, so each row leaves the listing as it found it.
        for key in written.keys() {
            let removed = s3(&router, "DELETE", &format!("/{markers}/{key}"), "").await;
            assert_eq!(
                removed.status, 204,
                "case {case:?}: deleting the row's record must succeed, got {} with {}",
                removed.status, removed.body
            );
        }
    }
}

/// RULE, for a message another SERVICE puts on the queue: the producer is no part of the rule,
/// so an object-created notification Amazon S3 publishes to the mapped queue is handed to the
/// function exactly as a client's own `SendMessage` is, and the handler's effect reaches S3.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn an_object_notification_on_a_mapped_queue_reaches_the_function() {
    driven("lambda-sqs-esm-s3-notification-chain", || {
        Box::pin(an_object_notification_on_a_mapped_queue_reaches_the_function_body())
    });
}

async fn an_object_notification_on_a_mapped_queue_reaches_the_function_body() {
    let case = "an object-created notification routed through the mapped queue";
    let router = build_router();
    let markers = "esm-notification-markers";
    let uploads = "esm-notification-uploads";
    create_bucket(&router, case, markers).await;
    create_bucket(&router, case, uploads).await;
    let (url, arn) = given_queue(&router, case, "esm-notification-source").await;
    sqs(
        &router,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": { "Policy": notification_grant(&arn, uploads) },
        }),
    )
    .await;
    put_queue_notification(&router, case, uploads, &arn).await;
    // The probe the install sends is drained as a client, so the write below is observed alone.
    awaited_test_event(&router, case, &url).await;

    let function = deployed_consumer(
        &router,
        case,
        "esm-notification-consumer",
        &consumer_stack(markers, NOTHING_FAILS),
    )
    .await;
    let uuid = given_mapping(&router, case, &function, &arn, 10, &[]).await;

    let key = "incoming/report.csv";
    let written = s3(
        &router,
        "PUT",
        &format!("/{uploads}/{key}"),
        "the object whose event drives the chain",
    )
    .await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: PutObject({uploads:?}, {key:?}) must succeed, got {} with {}",
        written.status, written.body
    );

    let records = awaited_records(&router, case, markers, 1).await;
    let record = records
        .values()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no record was written"));
    assert_eq!(
        record["eventSource"].as_str(),
        Some(SQS_EVENT_SOURCE),
        "case {case:?}: the record must name the queue as its event source, got {record}"
    );
    assert_eq!(
        record["eventSourceARN"].as_str(),
        Some(arn.as_str()),
        "case {case:?}: the record must name the mapped queue, got {record}"
    );
    let body = record["body"].as_str().unwrap_or_default();
    let published: Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: the body must be the notification S3 published ({error}), got {body:?}")
    });
    let event = &published["Records"][0];
    assert_eq!(
        event["eventSource"].as_str(),
        Some("aws:s3"),
        "case {case:?}: the record must carry S3's own notification, got {published}"
    );
    assert_eq!(
        event["s3"]["bucket"]["name"].as_str(),
        Some(uploads),
        "case {case:?}: the notification must name the written bucket, got {published}"
    );
    assert_eq!(
        event["s3"]["object"]["key"].as_str(),
        Some(key),
        "case {case:?}: the notification must name the written key, got {published}"
    );

    delete_mapping(&router, case, &uuid).await;
    let back = awaited_empty_queue(&router, case, &url).await;
    assert!(
        back.is_empty(),
        "case {case:?}: the notification the invocation processed must be deleted from the queue, \
         {url} handed back {back:?}"
    );
}

/// RULE, for a function that reports a partial batch: a mapping carrying
/// `ReportBatchItemFailures` leaves exactly the `itemIdentifiers` the response named for
/// redelivery once the queue's `VisibilityTimeout` lapses, and deletes the rest of the batch, so
/// no record beside the reported one is processed twice.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_partial_batch_report_redelivers_only_the_records_it_named() {
    driven("lambda-sqs-esm-partial-batch-report", || {
        Box::pin(a_partial_batch_report_redelivers_only_the_records_it_named_body())
    });
}

async fn a_partial_batch_report_redelivers_only_the_records_it_named_body() {
    let case = "one record of a batch reported as failed";
    let failing = "fail-this-record";
    let taken = ["taken-a", "taken-b"];
    let router = build_router();
    let markers = "esm-partial-batch-markers";
    create_bucket(&router, case, markers).await;
    let (url, arn) = given_queue(&router, case, "esm-partial-batch-source").await;
    let function = deployed_consumer(
        &router,
        case,
        "esm-partial-batch-consumer",
        &consumer_stack(markers, failing),
    )
    .await;
    let uuid = given_mapping(
        &router,
        case,
        &function,
        &arn,
        10,
        &[REPORT_BATCH_ITEM_FAILURES],
    )
    .await;

    let mut ids: BTreeMap<&str, String> = BTreeMap::new();
    for body in [taken[0], failing, taken[1]] {
        let sent = sqs(
            &router,
            case,
            "SendMessage",
            &json!({ "QueueUrl": url, "MessageBody": body }),
        )
        .await;
        let id = sent["MessageId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: SendMessage reports no MessageId, got {sent}"))
            .to_string();
        ids.insert(body, id);
    }
    let failing_id = ids[failing].clone();

    // The reported record comes back once the hold on it lapses, which is the SECOND delivery of
    // it the handler records.
    let deadline = Instant::now() + BUDGET;
    let written = loop {
        let written = written_records(&router, case, markers).await;
        if attempts_of(&written, &failing_id).len() >= 2 {
            break written;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the record the response reported must be handed over again, the \
             handler recorded {:?}",
            written.keys().collect::<Vec<&String>>()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };

    for body in taken {
        let id = &ids[body];
        assert_eq!(
            attempts_of(&written, id), vec![1],
            "case {case:?}: a record the response did not report is processed once, got the \
             deliveries {:?} for {body} ({id})",
            attempts_of(&written, id)
        );
    }
    let reported = attempts_of(&written, &failing_id);
    assert!(
        reported.len() >= 2 && reported[0] == 1,
        "case {case:?}: the reported record must be handed over again under a higher delivery \
         count, got {reported:?}"
    );

    // With the mapping gone nothing polls the queue: once no batch of it is in flight any more,
    // only the reported record is still on it, so the records the response did not name were
    // deleted.
    delete_mapping(&router, case, &uuid).await;
    awaited_nothing_in_flight(&router, case, &url).await;
    let deadline = Instant::now() + BUDGET;
    let back = loop {
        let back = drained(&router, case, &url).await;
        if !back.is_empty() {
            break back;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the reported record must come back to the queue, {url} handed out \
             nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        back,
        vec![failing.to_string()],
        "case {case:?}: exactly the reported record stays on the queue, got {back:?}"
    );
}
