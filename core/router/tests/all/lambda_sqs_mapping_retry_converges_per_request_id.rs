// SPDX-License-Identifier: CC0-1.0

//! What an Amazon SQS event source mapping does when the function it hands a batch to REFUSES
//! that batch and then takes it on a later delivery, across the services that own the halves of
//! it: Amazon SQS holds the queue, the messages on it and the hold it keeps each delivered
//! message under, IAM answers for the execution role the poller reads and the handler writes as,
//! CloudFormation archives the inline handler into the function's deployment package, Lambda
//! POLLS the queue and RUNS that handler, and Amazon S3 holds the objects the handler itself
//! writes.
//!
//! The rule pinned here: **a mapping that reports `Enabled` over a queue reads that queue on its
//! own and keeps reading it, so an invocation that FAILED processes nothing of its batch — every
//! message of it stays on the queue and is handed to the function again once the queue's
//! `VisibilityTimeout` lapses, under a higher delivery count — and the first delivery the
//! function does take is the one that processes it, after which that message is deleted and
//! handed over no more; so a handler whose far-end effect is keyed by the identity the message
//! itself carries converges on exactly ONE such effect per message, however many deliveries it
//! took.**
//!
//! Five crates have to agree for that to hold (Amazon SQS records the messages and decides when
//! a held one is visible again, IAM answers for the execution role, CloudFormation turns the
//! inline source into a runnable package, Lambda polls and invokes, Amazon S3 holds the effect),
//! so the suite lives here rather than in any one of them: a crate-local suite can see the poll
//! or the invocation, never the effect the handler had in a third service. Every observation is
//! one a client makes — `ListObjectsV2` and `GetObject` on the bucket the handler writes,
//! `GetQueueAttributes` and `ReceiveMessage` on the source queue, and Lambda's own
//! `GetEventSourceMapping`.
//!
//! The handler reports before it refuses: for every record it is handed it writes one object
//! keyed by that record's own identity AND the delivery count the queue reports for it, and only
//! then decides whether to raise. That is what tells "the mapping never invoked anything" (no
//! object at all) from "it invoked, the handler refused, and the message came back" (one object
//! per delivery), and it is the delivery history the convergence is asserted against.
//!
//! The rows differ along what the rule generalises over — how many messages are waiting, how many
//! of each message's deliveries the handler refuses before taking it, and how large a batch the
//! mapping asks for.
//!
//! The siblings pin the neighbouring halves: [`crate::lambda_sqs_event_source_drains_its_queue_into_s3`]
//! pins what a mapping hands a function that takes its batch first time, and
//! [`crate::lambda_sqs_event_source_failed_batch_keeps_its_message`] pins what a WHOLLY failed
//! invocation leaves behind. What is reached only here is the CONVERGENCE: a refused batch and a
//! later accepted one leaving exactly one far-end effect per message.
//!
//! The wire harnesses are the ones that already exist: the Amazon SQS and AWS Lambda calls, the
//! queue and the wait for a mapping to settle come from
//! [`crate::lambda_sqs_event_source_batching_window`], and the S3 request harness, the sized
//! thread and the stack reader from [`crate::s3_notification_invokes_its_lambda_target`] and
//! [`wait_for_stack_status`], so the way a client reaches this emulator is described in one place.
//!
//! Every request is dispatched through a boxed future on a thread sized like the emulator's own
//! boot path ([`driven`]): the chain this drives — Lambda's poll of the queue and the handler's
//! own SDK call back into Amazon S3 — nests more future than a default test thread's stack holds.

use std::collections::{BTreeMap, BTreeSet};
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

/// The `StackStatus` the consumer stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long the source queue hides a message it hands a consumer, in seconds, and the mapped
/// function's own `Timeout`: Lambda takes a queue mapping only while the queue's
/// `VisibilityTimeout` is at least the function's `Timeout`, and this is the span after which a
/// message no invocation processed is visible again.
const HOLD_SECONDS: i64 = 3;

/// The bucket every row's handler writes into, and the two prefixes it keys its objects under:
/// one object per DELIVERY the handler was given, and one per message it finally took.
const MARKER_BUCKET: &str = "esm-retry-convergence-markers";
const ATTEMPTS_PREFIX: &str = "attempts/";
const ANALYTICS_PREFIX: &str = "analytics/";

/// How long an observation of work the CLOCK owes — the stack coming up, the function going
/// `Active`, the mapping settling, an invocation, a hold lapsing, a redelivery — keeps looking
/// before giving up, and how long between two looks. The deadline of a CONDITION, not a
/// measurement of it: well above what the work takes even on a host running every suite of this
/// workspace at once, so a row fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(150);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a check that something must NOT happen keeps looking, once the queue reports nothing
/// waiting and nothing in flight and the mapping that polled it is gone: a redelivery the rule
/// forbids has every chance to show up, because nothing is holding a message and nothing is
/// polling for one.
const QUIET_WINDOW: Duration = Duration::from_secs(2);

/// The attribute that says how many of a queue's messages a consumer may be handed right now:
/// "ApproximateNumberOfMessages" (`GetQueueAttributes$AttributeNames`). Its in-flight
/// counterpart is the sibling suite's [`IN_FLIGHT_MESSAGES`].
const VISIBLE_MESSAGES: &str = "ApproximateNumberOfMessages";

/// The handler the one consumer below deploys inline.
///
/// For every record of the event it writes one object naming that record's own request id and the
/// delivery count the queue reports for it — so the deliveries it was given are a history a
/// client can read — and only THEN decides: a record still inside the number of deliveries its
/// own payload says to refuse makes the whole invocation raise, and otherwise the handler writes
/// the one object the work is keyed by, `analytics/<request id>.json`.
///
/// The number of deliveries to refuse travels in the MESSAGE rather than in the function's
/// environment, so every row below is served by this one deployed consumer and the rows differ
/// only in what they send.
const RETRYING_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    client = boto3.client("s3")
    bucket = os.environ["MARKER_BUCKET"]
    refused = []
    for record in event["Records"]:
        payload = json.loads(record["body"])
        attempt = int(record["attributes"]["ApproximateReceiveCount"])
        client.put_object(
            Bucket=bucket,
            Key="attempts/" + payload["requestId"] + "-" + str(attempt),
            Body=json.dumps(
                {
                    "requestId": payload["requestId"],
                    "messageId": record["messageId"],
                    "attempt": attempt,
                }
            ).encode("utf-8"),
        )
        if attempt <= int(payload["failUntil"]):
            refused.append(payload["requestId"])
    if refused:
        raise RuntimeError("this delivery is one the handler refuses: " + ",".join(sorted(refused)))
    for record in event["Records"]:
        payload = json.loads(record["body"])
        attempt = int(record["attributes"]["ApproximateReceiveCount"])
        client.put_object(
            Bucket=bucket,
            Key="analytics/" + payload["requestId"] + ".json",
            Body=json.dumps(
                {
                    "requestId": payload["requestId"],
                    "messageId": record["messageId"],
                    "attempt": attempt,
                }
            ).encode("utf-8"),
        )
    return {}
"#;

/// One Amazon SQS call that must answer 200. The wire form is the sibling suite's; what is added
/// here is that the call SUCCEEDED, since no step of this chain is a refusal.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = sqs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The stack the one consumer is deployed as: the execution role Lambda reads the event source
/// as and the handler writes its objects as, and the function whose inline code CloudFormation
/// archives into its deployment package.
fn consumer_stack() -> String {
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
                    // writes the handler makes.
                    "Policies": [{
                        "PolicyName": "poll-the-queue-and-write-the-analytics",
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
                    "MemorySize": 256,
                    "Environment": { "Variables": { "MARKER_BUCKET": MARKER_BUCKET } },
                    "Code": { "ZipFile": RETRYING_HANDLER },
                },
            },
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "Consumer" } } },
    })
    .to_string()
}

/// Deploy `stack`, wait for it to come up, and answer the name Lambda holds the function under
/// once it reports the function `Active`.
async fn deployed_consumer(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let template = consumer_stack();
    let (status, body) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
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
        .unwrap_or_else(|| {
            panic!("case {case:?}: the stack publishes no FunctionName, got {described}")
        });
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

/// The mapping of `function` onto `queue_arn`, asking for `batch_size` records per batch; answers
/// its `UUID` once Lambda reports it `Enabled`, which is the state a mapping polls its source in.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    queue_arn: &str,
    batch_size: i64,
) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        Some(&json!({
            "FunctionName": function,
            "EventSourceArn": queue_arn,
            "BatchSize": batch_size,
            "Enabled": true,
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {status} with \
         {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the created mapping reports no UUID, got {created}")
        })
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

/// Every object the handler has written under `prefix`, keyed by the key it wrote it under and
/// read back as the document it is.
async fn written_under(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
) -> BTreeMap<String, Value> {
    let listed = s3(
        router,
        "GET",
        &format!("/{MARKER_BUCKET}?list-type=2&prefix={prefix}"),
        "",
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListObjectsV2({MARKER_BUCKET:?}, {prefix:?}) must succeed, got {} with {}",
        listed.status, listed.body
    );
    let mut found = BTreeMap::new();
    for key in elements(&listed.body, "Key") {
        let object = s3(router, "GET", &format!("/{MARKER_BUCKET}/{key}"), "").await;
        assert_eq!(
            object.status, 200,
            "case {case:?}: GetObject({MARKER_BUCKET:?}, {key:?}) must succeed, got {} with {}",
            object.status, object.body
        );
        let parsed: Value = serde_json::from_str(&object.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the object the handler wrote must be a JSON document ({error}), \
                 got {}",
                object.body
            )
        });
        found.insert(key, parsed);
    }
    found
}

/// Look under `prefix` until it holds at least `wanted` objects, and answer what it holds —
/// bounded by [`BUDGET`], which is the condition's deadline rather than a measurement of it.
async fn awaited_under(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    wanted: usize,
) -> BTreeMap<String, Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let found = written_under(router, case, prefix).await;
        if found.len() >= wanted {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} object(s) under {prefix:?}, the handler wrote {:?}",
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

/// Wait until the queue at `url` holds NOTHING — neither a message a consumer may be handed nor
/// one in flight with one — and then keep looking for [`QUIET_WINDOW`], answering every body that
/// came back in that time.
///
/// The deletion of the batch the accepted invocation processed is what a client sees as an empty
/// queue, so the wait is on that CONDITION; a pass still working on a batch holds its messages in
/// flight, which is why the in-flight count is waited on too. Once both are zero nothing is
/// holding anything, so the window that follows says that nothing came BACK — with the mapping
/// already deleted, nothing is polling to put it there.
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
            "case {case:?}: the message the accepted invocation processed must be deleted from \
             the queue, {url} still reports {visible:?} waiting and {in_flight:?} in flight"
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

/// The delivery counts `objects` records for `request_id`, in ascending order.
///
/// The handler keys one object per delivery it was given, so this is the delivery HISTORY of that
/// message as the handler saw it. A missing count is a delivery that never reached the function.
fn attempts_of(objects: &BTreeMap<String, Value>, request_id: &str) -> Vec<u64> {
    let mut attempts: Vec<u64> = objects
        .values()
        .filter(|record| record["requestId"].as_str() == Some(request_id))
        .map(|record| record["attempt"].as_u64().unwrap_or_default())
        .collect();
    attempts.sort_unstable();
    attempts
}

/// One row: what the row sends and what the row's mapping asks for.
struct Row {
    case: &'static str,
    /// The slug the row's queue and its request ids are named from, which scopes the row's own
    /// objects inside the shared bucket.
    slug: &'static str,
    /// How many messages the row sends, one request id each.
    messages: usize,
    /// How many of each message's deliveries the handler refuses before taking it.
    refusals: u64,
    /// `BatchSize` the row's mapping asks for — "the maximum number of records in each batch",
    /// `@range min 1 max 10000` (`smithy_query(query="operation",
    /// operation="CreateEventSourceMapping")`).
    batch_size: i64,
}

/// The rows, differing along what the rule generalises over: how many messages are waiting, how
/// many deliveries of each the handler refuses before taking it, and how large a batch the
/// mapping asks for.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one message the handler refuses once, one message per batch",
            slug: "esm-retry-once",
            messages: 1,
            refusals: 1,
            batch_size: 1,
        },
        Row {
            case: "three messages the handler refuses once each, one message per batch",
            slug: "esm-retry-three",
            messages: 3,
            refusals: 1,
            batch_size: 1,
        },
        Row {
            case: "two messages the handler refuses twice each, a batch of up to ten",
            slug: "esm-retry-twice",
            messages: 2,
            refusals: 2,
            batch_size: 10,
        },
    ]
}

/// RULE: an `Enabled` mapping over a queue keeps reading it, so an invocation that failed
/// processes nothing of its batch and every message of it comes back once the queue's
/// `VisibilityTimeout` lapses and is handed over again under a higher delivery count; the first
/// delivery the function takes processes it, after which the message is deleted and handed over
/// no more — so a handler whose far-end effect is keyed by the message's own identity leaves
/// exactly one such effect per message, however many deliveries it took.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_refused_batch_comes_back_and_its_retry_leaves_one_effect_per_request_id() {
    driven("lambda-sqs-esm-retry-convergence", || {
        Box::pin(a_refused_batch_comes_back_and_its_retry_leaves_one_effect_per_request_id_body())
    });
}

async fn a_refused_batch_comes_back_and_its_retry_leaves_one_effect_per_request_id_body() {
    let router = build_router();
    let setup = "the shared wiring";
    create_bucket(&router, setup, MARKER_BUCKET).await;
    let function = deployed_consumer(&router, setup, "esm-retry-convergence-consumer").await;

    for row in rows() {
        let case = row.case;
        let (url, arn) =
            given_queue_hiding_for(&router, case, &format!("{}-source", row.slug), HOLD_SECONDS)
                .await;
        let uuid = given_mapping(&router, case, &function, &arn, row.batch_size).await;

        // Everything the row sends is on the queue before anything is read, so the whole of what
        // was waiting is what the mapping had to hand over. The request id is the identity the
        // far-end effect is keyed by, and it travels in the message the client sends.
        let mut sent: Vec<(String, String)> = Vec::new();
        for index in 1..=row.messages {
            let request_id = format!("{}-{index}", row.slug);
            let body = json!({ "requestId": request_id, "failUntil": row.refusals }).to_string();
            let answered = sqs(
                &router,
                case,
                "SendMessage",
                &json!({ "QueueUrl": url, "MessageBody": body }),
            )
            .await;
            let message_id = answered["MessageId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: SendMessage reports no MessageId, got {answered}")
                })
                .to_string();
            sent.push((request_id, message_id));
        }

        // The far-end effect of every message, once the mapping has driven each of them through
        // the refusals and on to a delivery the handler took.
        let analytics_prefix = format!("{ANALYTICS_PREFIX}{}-", row.slug);
        let analytics = awaited_under(&router, case, &analytics_prefix, row.messages).await;
        let expected_keys: BTreeSet<String> = sent
            .iter()
            .map(|(request_id, _)| format!("{ANALYTICS_PREFIX}{request_id}.json"))
            .collect();
        let found_keys: BTreeSet<String> = analytics.keys().cloned().collect();
        assert_eq!(
            found_keys, expected_keys,
            "case {case:?}: the mapping must leave exactly one object per request id, it left \
             {found_keys:?}"
        );

        for (request_id, message_id) in &sent {
            let key = format!("{ANALYTICS_PREFIX}{request_id}.json");
            let effect = analytics
                .get(&key)
                .unwrap_or_else(|| panic!("case {case:?}: nothing was written under {key:?}"));
            assert_eq!(
                effect["requestId"].as_str(),
                Some(request_id.as_str()),
                "case {case:?}: the object must name the request id it is keyed by, got {effect}"
            );
            assert_eq!(
                effect["messageId"].as_str(),
                Some(message_id.as_str()),
                "case {case:?}: the object must name the message the mapping handed over, got \
                 {effect}"
            );
            // The effect happened on the delivery AFTER the refused ones: the handler took the
            // batch only once the queue had handed it over `refusals` times already.
            assert_eq!(
                effect["attempt"].as_u64(),
                Some(row.refusals + 1),
                "case {case:?}: expected the effect of {request_id} to be written on delivery \
                 {}, got {effect}",
                row.refusals + 1
            );
        }

        // Nothing polls the queue once the mapping is gone, so what the queue holds is settled
        // and the client's to read: the message the accepted invocation processed was deleted,
        // and nothing comes back.
        delete_mapping(&router, case, &uuid).await;
        let back = awaited_empty_queue(&router, case, &url).await;
        assert!(
            back.is_empty(),
            "case {case:?}: every message a delivery the handler took processed must be deleted \
             from the queue, {url} handed back {back:?}"
        );

        // The delivery HISTORY the handler recorded: every message was handed over once per
        // refusal and then exactly once more, which is both halves of the rule — the refused
        // batch really came back, and the delivery that was taken was the last one.
        let attempts_prefix = format!("{ATTEMPTS_PREFIX}{}-", row.slug);
        let attempts = written_under(&router, case, &attempts_prefix).await;
        let expected_attempts: Vec<u64> = (1..=row.refusals + 1).collect();
        for (request_id, _) in &sent {
            let seen = attempts_of(&attempts, request_id);
            assert_eq!(
                seen, expected_attempts,
                "case {case:?}: expected {request_id} to be handed to the function on the \
                 deliveries {expected_attempts:?}, got {seen:?}"
            );
        }
    }
}
