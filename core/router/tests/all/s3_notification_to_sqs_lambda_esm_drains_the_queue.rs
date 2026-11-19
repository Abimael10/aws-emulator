// SPDX-License-Identifier: CC0-1.0

//! The THREE-service pipeline an object write drives when a bucket notifies a QUEUE that a
//! Lambda event source mapping consumes: Amazon S3 evaluates the bucket's notification
//! configuration and publishes the object event, Amazon SQS holds the published message, and
//! Lambda polls that queue and RUNS the function the mapping names.
//!
//! The rule pinned here: **a bucket's notification configuration whose destination is a queue
//! publishes each object event its `Events` and its key-name `Filter` admit to that queue as
//! one message whose body is the event document, and an `Enabled` event source mapping on the
//! queue hands those messages to its function as `Records[]` and deletes each message the
//! invocation processed — so an object write reaches the mapped function as an SQS record
//! carrying the S3 event for the written key, and the queue drains; a key the filter does not
//! admit is published nowhere and so reaches that function not at all.**
//!
//! Three crates have to agree for that to hold (S3 decides and publishes, Amazon SQS holds the
//! message and counts the delivery, Lambda polls and invokes, with CloudFormation archiving the
//! inline handler into the deployment package and IAM answering for the execution role), so the
//! suite lives here: a crate-local suite can only ever see one leg. Its siblings pin the legs
//! one at a time — [`crate::s3_notification_fans_out_across_destination_kinds`] pins what one
//! configuration publishes to each destination KIND,
//! [`crate::s3_notification_delivery_follows_the_destination_grant`] pins the grant each
//! publication is decided by, and [`crate::lambda_sqs_event_source_failed_batch_keeps_its_message`]
//! pins what a queue mapping hands its function and what a failed attempt leaves behind. What
//! only this suite reaches is the JOIN: an event published by one service, consumed by another's
//! poller and handed to a handler that really runs.
//!
//! Every observation is one a client makes: the markers the handler forwarded to a queue of its
//! own, and the source queue's own message counts. Each wait is on a CONDITION — a key reported,
//! the counts at zero — with a generous deadline, so a loaded host delays a row instead of
//! weakening it.
//!
//! The rows differ along what the rule generalises over: whether the configuration carries a
//! key-name filter at all, how many objects are written before anything is read (so a batch may
//! span several records), how many records one batch may hold (`BatchSize`), and whether a
//! written key is one the filter admits.
//!
//! Every request is dispatched through a BOXED future on a thread sized like the emulator's own
//! boot path ([`driven`]): this chain — S3's publication into Amazon SQS, the poller's
//! invocation into Lambda, and the handler's own SDK call back into Amazon SQS — nests more
//! future than a default test thread's stack holds.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    ABANDON_GRACE, HOST_WAIT_CEILING, INIT_PHASE_LIMIT, PERMIT_WAIT,
};

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::s3_notification_invokes_its_lambda_target::{
    authorization, build_router, create_bucket, driven, query, s3,
};

/// The Region every request below is signed for, which is the Region every resource of the
/// pipeline lives in. The shared stack reader ([`wait_for_stack_status`]) signs for the same
/// one.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The event class every configuration below is notified of: "you can enable notifications for
/// every event of a type by using the wildcard" (`Event`).
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The service principal Amazon S3 publishes a bucket's notifications under, and the action it
/// performs on a queue destination.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The `StackStatus` each row's handler stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where `CreateEventSourceMapping` is bound (`POST /2015-03-31/event-source-mappings`).
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `State` a mapping reports once it polls its event source, from
/// `EventSourceMappingConfiguration$State`.
const STATE_ENABLED: &str = "Enabled";

/// The `State` a function serves invocations in ("the function is ready to be invoked",
/// `FunctionConfiguration$State`).
const FUNCTION_ACTIVE: &str = "Active";

/// `eventSource` every record of a QUEUE-sourced event carries, and the one every record of an
/// S3 object event carries.
const SQS_EVENT_SOURCE: &str = "aws:sqs";
const S3_EVENT_SOURCE: &str = "aws:s3";

/// The `eventName` an object PUT is published under.
const OBJECT_PUT: &str = "ObjectCreated:Put";

/// How long each source queue hides a message it hands a consumer, in seconds, and the
/// `Timeout` of the function it maps onto: Lambda takes a queue mapping only while the queue's
/// `VisibilityTimeout` is at least the function's `Timeout`.
const VISIBILITY_TIMEOUT: i64 = 60;
/// See [`VISIBILITY_TIMEOUT`].
const FUNCTION_TIMEOUT: i64 = 30;

/// How long an observation of work the pipeline owes — a stack's resources coming up, a
/// function going `Active`, a publication, a poll, an invocation, a delete — waits before it is
/// a failure, and how long between two looks.
///
/// The bound is PATIENCE, not a weakened expectation: every assertion below is unchanged. It is
/// taken from what the EMULATOR itself allows ONE invocation rather than being picked here — the
/// wait for a share of the run capacity, the initialization of the execution environment, the
/// longest the run is waited for and the grace past it
/// (`service_com_amazonaws_lambda_awsgirapiservice::state::execution`) — doubled for the several
/// invocations a row's objects drive, with a minute on top because a whole-workspace test run
/// shares its cores with every other suite. A healthy row settles in seconds, so the deadline is
/// what a BROKEN pipeline costs before it is reported, and it is kept to about that rather than
/// stretched further.
const BUDGET: Duration = PERMIT_WAIT
    .saturating_add(INIT_PHASE_LIMIT)
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(ABANDON_GRACE)
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(60));
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How long a row keeps looking for a marker that must NOT be there, once every marker that
/// must be there has arrived: by then the pipeline is known to be live, so the window is read
/// against a working one.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// The body every object below is written with. Nothing reads it: what the handler reports is
/// the record it was handed.
const OBJECT_BODY: &str = "the object the notification is published for";

/// The handler every row deploys, whose inline source CloudFormation archives into the
/// function's deployment package.
///
/// For each record of the batch it is handed it reads the event document out of that record's
/// `body` and forwards one marker per S3 record to the queue its environment names — addressed
/// by NAME through Amazon SQS's own `GetQueueUrl`, so the source carries nothing the deployment
/// had to compute. Each marker also reports how many records the BATCH it came from held, which
/// is what the mapping's `BatchSize` bounds. A body carrying no `Records` (the test notification
/// a validated queue destination receives when a configuration is installed) is reported as
/// nothing and processed, so it leaves the queue like every other message.
const FORWARDING_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName=os.environ["MARKER_QUEUE"])["QueueUrl"]
    reported = []
    held = len(event["Records"])
    for record in event["Records"]:
        try:
            envelope = json.loads(record["body"])
        except ValueError:
            continue
        for published in envelope.get("Records", []):
            marker = {
                "batchRecords": held,
                "sqsEventSource": record["eventSource"],
                "sqsEventSourceARN": record["eventSourceARN"],
                "s3EventSource": published["eventSource"],
                "eventName": published["eventName"],
                "bucket": published["s3"]["bucket"]["name"],
                "key": published["s3"]["object"]["key"],
            }
            client.send_message(QueueUrl=url, MessageBody=json.dumps(marker))
            reported.append(marker["key"])
    return {"reported": reported}
"#;

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One AWS Lambda call over its `restJson1` wire, as its status and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(
            body.map(|body| body.to_string()).unwrap_or_default(),
        ))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A queue called `name`, hiding a delivered message for `visibility_timeout` seconds; answers
/// its url and the ARN Amazon SQS itself reports for it.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    visibility_timeout: i64,
) -> (String, String) {
    let created = sqs(
        router,
        case,
        "CreateQueue",
        &json!({
            "QueueName": name,
            "Attributes": { "VisibilityTimeout": visibility_timeout.to_string() },
        }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue {name:?} reports no QueueUrl"))
        .to_string();
    let read = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = read["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue {name:?} reports no QueueArn"))
        .to_string();
    (url, arn)
}

/// Grant the Amazon S3 service principal the publication on the queue at `url`, narrowed by
/// `aws:SourceArn` to the publishing bucket: the grant a notification destination's own policy
/// has to state for Amazon S3 to publish to it. An S3 bucket's ARN carries no Region and no
/// account, because a bucket name is globally unique.
async fn allow_publications(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    queue_arn: &str,
    bucket: &str,
) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowS3Notifications",
            "Effect": "Allow",
            "Principal": { "Service": S3_PRINCIPAL },
            "Action": SEND_MESSAGE_ACTION,
            "Resource": queue_arn,
            "Condition": { "ArnLike": { "aws:SourceArn": format!("arn:aws:s3:::{bucket}") } },
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
}

/// `PUT /<bucket>?notification` with ONE queue configuration naming `queue_arn` for
/// `s3:ObjectCreated:*`, scoped to `prefix` when the row carries one.
///
/// The elements are the ones S3's own model binds the queue list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`: `QueueConfigurations` is
/// bound as `QueueConfiguration`, whose ARN member is bound as `Queue`).
async fn put_queue_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    queue_arn: &str,
    prefix: Option<&str>,
) {
    let filter = prefix
        .map(|prefix| {
            format!(
                "<Filter><S3Key><FilterRule><Name>prefix</Name><Value>{prefix}</Value>\
                 </FilterRule></S3Key></Filter>"
            )
        })
        .unwrap_or_default();
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><QueueConfiguration>\
         <Id>to-the-consumer</Id><Queue>{queue_arn}</Queue><Event>{OBJECT_CREATED}</Event>\
         {filter}</QueueConfiguration></NotificationConfiguration>"
    );
    let answer = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the notification configuration must be accepted while the queue grants \
         Amazon S3 the publication, got {} with {}",
        answer.status, answer.body
    );
}

/// `PUT /<bucket>/<key>` — the object write whose event the configuration publishes.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        OBJECT_BODY,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// The stack each row deploys: the execution role the poller reads and the handler writes as,
/// and the function whose inline code CloudFormation archives into its deployment package.
fn consumer_stack(marker_queue: &str) -> String {
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
                    // The reads the mapping's poller makes on the source queue, and the
                    // writes the handler makes to its marker queue.
                    "Policies": [{
                        "PolicyName": "consume-and-report",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "sqs:ReceiveMessage",
                                    "sqs:DeleteMessage",
                                    "sqs:GetQueueAttributes",
                                    "sqs:GetQueueUrl",
                                    "sqs:SendMessage",
                                    "logs:*",
                                ],
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
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { "MARKER_QUEUE": marker_queue } },
                    "Code": { "ZipFile": FORWARDING_HANDLER },
                },
            },
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": "Consumer" } },
        },
    })
    .to_string()
}

/// Deploy that stack and answer the function's name, once Lambda reports the function `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    marker_queue: &str,
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &consumer_stack(marker_queue)),
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
        "case {case:?}: the consumer's stack must reach {CREATE_COMPLETE}, got {described}"
    );
    let function = outputs_of(&described)
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the function's name"));
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
            "case {case:?}: Lambda never reported {function:?} {FUNCTION_ACTIVE}, last read \
             {status} {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The mapping between the row's source queue and its function, answered once Lambda reports it
/// `Enabled` — the state in which "the event source mapping is active".
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
        Some(json!({
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
        .unwrap_or_else(|| panic!("case {case:?}: the created mapping reports no UUID"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        if status == 200 && read["State"].as_str() == Some(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} never reported {STATE_ENABLED}, last read \
             {status} {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Take whatever the handler has forwarded to the marker queue at `url`, deleting each message
/// so a later look finds only later markers, and add each marker to `seen` under the key it
/// names.
async fn take_markers(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    seen: &mut BTreeMap<String, Value>,
) {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        let text = message["Body"].as_str().unwrap_or_default().to_string();
        if let Some(receipt) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": receipt }),
            )
            .await;
        }
        let marker: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a forwarded marker as JSON, got {text:?} ({error})")
        });
        let key = marker["key"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a forwarded marker naming no key: {marker}"))
            .to_string();
        seen.insert(key, marker);
    }
}

/// Look at the marker queue until every key of `wanted` has been reported, answering every
/// marker seen. Bounded by [`BUDGET`], which is the CONDITION's deadline and not a measurement
/// of it.
async fn awaited_markers(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: &[&str],
) -> BTreeMap<String, Value> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: BTreeMap<String, Value> = BTreeMap::new();
    loop {
        take_markers(router, case, url, &mut seen).await;
        if wanted.iter().all(|key| seen.contains_key(*key)) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the mapped function to be handed the event of every one of \
             {wanted:?}, it reported {:?}",
            seen.keys().collect::<BTreeSet<&String>>()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// How many messages the queue at `url` still holds, waiting and in flight: a message a
/// successful invocation processed is deleted, so a drained pipeline leaves none of either.
///
/// Both counts are read as the attributes Amazon SQS reports them in and neither is DEFAULTED:
/// an answer that stopped reporting one would fail the row rather than read as a drained queue.
async fn messages_held(router: &MultiServiceRouter, case: &str, url: &str) -> (u64, u64) {
    let read = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    )
    .await;
    let count = |name: &str| -> u64 {
        let reported = read["Attributes"][name].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the queue must report {name}, got {read}")
        });
        reported.parse().unwrap_or_else(|error| {
            panic!("case {case:?}: {name} must be a count, got {reported:?} ({error})")
        })
    };
    (
        count("ApproximateNumberOfMessages"),
        count("ApproximateNumberOfMessagesNotVisible"),
    )
}

/// Wait until the queue at `url` holds nothing at all, answering the last counts it reported.
async fn awaited_drain(router: &MultiServiceRouter, case: &str, url: &str) -> (u64, u64) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let counts = messages_held(router, case, url).await;
        if counts == (0, 0) || Instant::now() >= deadline {
            return counts;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: the key-name prefix the configuration is scoped to, the keys written, which of them
/// the configuration admits, and how many records one batch may hold.
struct Row {
    case: &'static str,
    slug: &'static str,
    prefix: Option<&'static str>,
    written: &'static [&'static str],
    delivered: &'static [&'static str],
    withheld: &'static [&'static str],
    batch_size: i64,
}

/// The rows.
///
/// The first row writes MORE objects than its `BatchSize` admits, so the bound each marker is
/// read against is one the row could cross: "the maximum number of records in each batch that
/// Lambda pulls from your stream or queue" — a mapping that handed its function everything
/// waiting would be caught by it, while the second row's single record per batch pins the floor.
const ROWS: &[Row] = &[
    Row {
        case: "no key filter, three objects written before anything is read, two per batch",
        slug: "s3q-any",
        prefix: None,
        written: &["intake/alpha.txt", "intake/beta.txt", "plain.txt"],
        delivered: &["intake/alpha.txt", "intake/beta.txt", "plain.txt"],
        withheld: &[],
        batch_size: 2,
    },
    Row {
        case: "a prefix filter, one record per batch, one admitted key beside an excluded one",
        slug: "s3q-prefix",
        prefix: Some("watched/"),
        written: &["watched/gamma.txt", "ignored/delta.txt"],
        delivered: &["watched/gamma.txt"],
        withheld: &["ignored/delta.txt"],
        batch_size: 1,
    },
];

/// RULE: a bucket notification whose destination is a queue publishes each admitted object
/// event onto that queue, and an enabled event source mapping on the queue hands those messages
/// to its function and deletes what the invocation processed — so the written key reaches the
/// mapped function as an SQS record carrying the S3 event, the queue drains, and a key the
/// filter excludes reaches the function not at all.
#[test]
fn an_object_write_reaches_the_function_mapped_onto_the_notified_queue() {
    driven("s3-notification-to-sqs-lambda-esm", || {
        Box::pin(async {
            // ONE emulator for the whole table: every row names its own bucket, queues,
            // function and stack after its own slug, so no row can observe another's.
            let router = build_router();
            for row in ROWS {
                let case = row.case;
                let bucket = format!("{}-bucket", row.slug);
                create_bucket(&router, case, &bucket).await;

                let marker_name = format!("{}-markers", row.slug);
                let (marker_url, _) = given_queue(&router, case, &marker_name, 1).await;
                let (source_url, source_arn) = given_queue(
                    &router,
                    case,
                    &format!("{}-source", row.slug),
                    VISIBILITY_TIMEOUT,
                )
                .await;
                allow_publications(&router, case, &source_url, &source_arn, &bucket).await;
                put_queue_notification(&router, case, &bucket, &source_arn, row.prefix).await;

                let function =
                    given_function(&router, case, &format!("{}-stack", row.slug), &marker_name)
                        .await;
                let uuid =
                    given_mapping(&router, case, &function, &source_arn, row.batch_size).await;

                for key in row.written {
                    put_object(&router, case, &bucket, key).await;
                }

                // Every admitted key reaches the HANDLER, as a record of a queue-sourced event
                // whose body is the document S3 published.
                let seen = awaited_markers(&router, case, &marker_url, row.delivered).await;
                for key in row.delivered {
                    let marker = &seen[*key];
                    assert_eq!(
                        marker["sqsEventSource"].as_str(),
                        Some(SQS_EVENT_SOURCE),
                        "case {case:?}: the record for {key:?} must be a queue-sourced one, got \
                         {marker}"
                    );
                    assert_eq!(
                        marker["sqsEventSourceARN"].as_str(),
                        Some(source_arn.as_str()),
                        "case {case:?}: the record for {key:?} must name the queue it came from, \
                         got {marker}"
                    );
                    assert_eq!(
                        marker["s3EventSource"].as_str(),
                        Some(S3_EVENT_SOURCE),
                        "case {case:?}: the published event of {key:?} must name its own source, \
                         got {marker}"
                    );
                    assert_eq!(
                        marker["eventName"].as_str(),
                        Some(OBJECT_PUT),
                        "case {case:?}: the published event of {key:?} must name the write that \
                         caused it, got {marker}"
                    );
                    assert_eq!(
                        marker["bucket"].as_str(),
                        Some(bucket.as_str()),
                        "case {case:?}: the published event of {key:?} must name the written \
                         bucket, got {marker}"
                    );
                    let batch_records = marker["batchRecords"].as_i64().unwrap_or_default();
                    assert!(
                        (1..=row.batch_size).contains(&batch_records),
                        "case {case:?}: the batch the record for {key:?} came in must hold \
                         between one record and the mapping's BatchSize of {}, got \
                         {batch_records}",
                        row.batch_size
                    );
                }

                // The messages the invocations processed are deleted, so the queue the
                // notifications landed on drains — the test notification a validated
                // destination received included.
                let counts = awaited_drain(&router, case, &source_url).await;
                assert_eq!(
                    counts,
                    (0, 0),
                    "case {case:?}: every message the mapping's invocations processed must be \
                     deleted, so the source queue must hold nothing, got {counts:?} as (waiting, \
                     in flight)"
                );

                // And a key the configuration's filter does not admit was published nowhere, so
                // the function was never handed it. The window is read after the pipeline has
                // been seen to work, and nothing is deleted from the source queue meanwhile.
                let mut after = seen;
                if !row.withheld.is_empty() {
                    let deadline = Instant::now() + QUIET_WINDOW;
                    while Instant::now() < deadline {
                        take_markers(&router, case, &marker_url, &mut after).await;
                        tokio::time::sleep(LOOK_AGAIN).await;
                    }
                }
                for key in row.withheld {
                    assert!(
                        !after.contains_key(*key),
                        "case {case:?}: {key:?} is not admitted by the configuration's filter, \
                         so no record of it can reach the function, got {:?}",
                        after.keys().collect::<BTreeSet<&String>>()
                    );
                }

                // The mapping is left where the row found it: still the enabled consumer of its
                // own queue, which is what every assertion above was made against.
                let (status, read) =
                    lambda(&router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
                assert_eq!(
                    (status, read["State"].as_str()),
                    (200, Some(STATE_ENABLED)),
                    "case {case:?}: the mapping must still report {STATE_ENABLED}, got {status} \
                     {read}"
                );
            }
        })
    });
}
