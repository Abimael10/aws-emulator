// SPDX-License-Identifier: CC0-1.0

//! One rule carrying BOTH an Amazon Data Firehose delivery-stream target and an Amazon SQS
//! queue target, and what each of them receives for a single `PutEvents`.
//!
//! The rule pinned here: **a matching event is handed to EVERY target of the rule
//! independently — EventBridge makes the call each target's service is invoked by
//! (`firehose:PutRecord` for a delivery stream, which the stream then writes to its Amazon S3
//! destination under the configured prefix; `sqs:SendMessage` for a queue) — and no target's
//! outcome decides another's, so a leg whose own authorization withholds the call delivers
//! nothing while every other leg of the same rule still delivers.**
//!
//! Which document decides each leg is the target kind's own: a delivery stream is one of the
//! kinds reached by ACTING AS the target's `RoleArn` ("for EC2 instances, Kinesis data
//! streams, Step Functions state machines and API Gateway APIs, EventBridge relies on IAM
//! roles that you specify in the `RoleARN` argument in `PutTargets`"), while a queue is one of
//! the kinds its own resource-based policy decides ("for Lambda functions, Amazon SNS topics
//! and Amazon SQS queues, EventBridge relies on resource-based policies"). `PutTargets` models
//! `Target{Id, Arn, RoleArn}`, so a delivery-stream ARN carrying a role and a queue ARN
//! carrying none are both accepted target configurations of one rule.
//!
//! Only a cross-service suite can pin this: the flow spans five services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them — EventBridge for the rule and the
//! publication, IAM for the role the stream leg is made under, Firehose for the stream, Amazon
//! S3 for the object its delivery writes, and Amazon SQS for the message the other leg
//! receives.
//!
//! The rows differ along what the rule generalises over: WHICH leg's authorization admits the
//! call (both of them, or only one), the FORM that authorization takes (an identity policy on
//! the target's role, a resource policy on the queue, no policy at all), and the ORDER the two
//! targets are written in.
//!
//! The Region, the wire encoder and the IAM role builder are the ones
//! `events_rule_firehose_target_delivery.rs` already holds for this same set of services, so
//! both suites sign for one Region and build a role one way.
//!
//! The publication's own values, the per-service wire helpers, the queue builder and the
//! waits below are `pub(crate)` in turn, so a suite driving the SAME fan-out over the same
//! five services — `events_rule_target_input_overrides_fan_out_independently.rs` — calls them
//! rather than restating them: one publication, one encoder and one set of waits for both.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::events_rule_firehose_target_delivery::{given_role, send, REGION};
use crate::query_wire::elements;

/// The bucket every delivery stream of this suite writes into.
const BUCKET: &str = "events-fanout-firehose-and-queue";

/// The role every STREAM delivers to that bucket under: Firehose has to be able to assume it,
/// and it has to allow the object write the delivery makes.
const DELIVERY_ROLE: &str = "fanout-firehose-delivery-role";

/// The source and detail type every rule below matches on, and the detail the single published
/// event carries.
pub(crate) const SOURCE: &str = "com.demo.fanout";
pub(crate) const DETAIL_TYPE: &str = "Order Teed";
pub(crate) const ORDER_ID: &str = "A-7";

/// The service principal EventBridge assumes a target's role as, and the one a queue's policy
/// has to admit for a queue leg to be delivered.
pub(crate) const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The call each leg performs, which that leg's own authorization has to admit.
pub(crate) const PUT_RECORD_ACTION: &str = "firehose:PutRecord";
pub(crate) const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The status a delivery stream has to report before a record may be put into it
/// (`DeliveryStreamDescription$DeliveryStreamStatus`).
const ACTIVE: &str = "ACTIVE";

/// How long anything the CLOCK owes — a stream leaving `CREATING`, a delivery, a queue
/// message — is waited for before a row fails: far longer than the emulator's own spans, so a
/// loaded machine does not fail a row the clock would have served. Nothing here waits a fixed
/// time: every wait looks again until the condition it waits on holds.
pub(crate) const DEADLINE: Duration = Duration::from_secs(60);

/// How long between two looks while waiting for one of those conditions.
pub(crate) const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The order one row writes its two targets in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// The delivery-stream target first, then the queue target.
    StreamFirst,
    /// The queue target first, then the delivery-stream target.
    QueueFirst,
}

/// One row: what each leg's own authorization admits, the order the two targets are written
/// in, and what each far end then holds.
struct Row {
    case: &'static str,
    /// Names this row's stream, destination prefix, queue, rule and target role, so rows
    /// cannot collide.
    name: &'static str,
    /// The action the delivery-stream target's `RoleArn` is allowed.
    stream_role_allows: &'static str,
    /// The action the queue's own resource policy grants `events.amazonaws.com`, or `None` for
    /// a queue carrying no policy at all.
    queue_policy_grants: Option<&'static str>,
    order: Order,
    /// Whether the stream's S3 destination holds the delivered envelope.
    expect_object: bool,
    /// Whether the queue holds the delivered envelope.
    expect_message: bool,
}

/// The table: every combination of the two legs' authorizations, and both target orders.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "both legs admitted, the stream target written first",
            name: "fanout-both",
            stream_role_allows: PUT_RECORD_ACTION,
            queue_policy_grants: Some(SEND_MESSAGE_ACTION),
            order: Order::StreamFirst,
            expect_object: true,
            expect_message: true,
        },
        Row {
            case: "both legs admitted, the same two targets in the opposite order",
            name: "fanout-reversed",
            stream_role_allows: PUT_RECORD_ACTION,
            queue_policy_grants: Some(SEND_MESSAGE_ACTION),
            order: Order::QueueFirst,
            expect_object: true,
            expect_message: true,
        },
        Row {
            case: "the stream target's role allows another action, the queue admits the send",
            name: "fanout-stream-barred",
            stream_role_allows: "firehose:DescribeDeliveryStream",
            queue_policy_grants: Some(SEND_MESSAGE_ACTION),
            order: Order::StreamFirst,
            expect_object: false,
            expect_message: true,
        },
        Row {
            case: "the queue's policy grants another action, the stream's role allows the put",
            name: "fanout-queue-barred",
            stream_role_allows: PUT_RECORD_ACTION,
            queue_policy_grants: Some("sqs:ReceiveMessage"),
            order: Order::QueueFirst,
            expect_object: true,
            expect_message: false,
        },
        Row {
            case: "the queue carries no policy at all, the stream's role allows the put",
            name: "fanout-queue-unpoliced",
            stream_role_allows: PUT_RECORD_ACTION,
            queue_policy_grants: None,
            order: Order::StreamFirst,
            expect_object: true,
            expect_message: false,
        },
    ]
}

/// A queue policy granting `events.amazonaws.com` the single `action` on `queue_arn`.
fn queue_policy(action: &str, queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": action,
            "Resource": queue_arn,
        }],
    })
    .to_string()
}

/// One `X-Amz-Target` RPC operation that must succeed, as its parsed answer.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    json_version: &str,
    target: &str,
    body: Value,
) -> Value {
    let (status, answered) = send(
        router,
        signing_name,
        "POST",
        "/",
        &[
            ("content-type", &format!("application/x-amz-json-{json_version}")),
            ("x-amz-target", target),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {target} must succeed, got {status} {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One EventBridge operation (`awsJson1_1`) that must succeed.
pub(crate) async fn events(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    rpc(
        router,
        case,
        "events",
        "1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await
}

/// One Firehose operation (`awsJson1_1`) that must succeed.
pub(crate) async fn firehose(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    rpc(
        router,
        case,
        "firehose",
        "1.1",
        &format!("Firehose_20150804.{operation}"),
        body,
    )
    .await
}

/// One Amazon SQS operation (`awsJson1_0`) that must succeed.
pub(crate) async fn sqs(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    rpc(
        router,
        case,
        "sqs",
        "1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await
}

/// The queue `name`, carrying the policy `grants` describes when one is named; answers its url
/// and the ARN a target names it by.
pub(crate) async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    grants: Option<&str>,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let arn = format!(
        "arn:aws:sqs:{REGION}:{}:{name}",
        engine::emulator_account_id()
    );
    if let Some(action) = grants {
        sqs(
            router,
            case,
            "SetQueueAttributes",
            json!({
                "QueueUrl": url,
                "Attributes": { "Policy": queue_policy(action, &arn) },
            }),
        )
        .await;
    }
    (url, arn)
}

/// Every key `bucket` holds under `prefix`, read through `ListObjectsV2`.
///
/// The bucket is a PARAMETER rather than this file's own, so a suite driving the same fan-out
/// into a bucket of its own calls this one wait on the Amazon S3 leg rather than restating it.
pub(crate) async fn keys_under(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing {prefix:?} must succeed, got {status} {text}"
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// Everything `bucket` holds under `prefix`, concatenated in key order, once at least one
/// object is there — or a failure naming the row once [`DEADLINE`] has passed.
pub(crate) async fn delivered_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let keys = keys_under(router, case, bucket, prefix).await;
        if !keys.is_empty() {
            let mut body = Vec::new();
            for key in &keys {
                let (status, bytes) =
                    send(router, "s3", "GET", &format!("/{bucket}/{key}"), &[], Vec::new()).await;
                assert_eq!(
                    status,
                    200,
                    "case {case:?}: reading {key:?} must succeed, got {status} {}",
                    String::from_utf8_lossy(&bytes)
                );
                body.extend_from_slice(&bytes);
            }
            return String::from_utf8_lossy(&body).to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stream leg's delivered object under {prefix:?}, got none"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Up to ten message bodies the queue at `url` holds, draining them.
pub(crate) async fn received(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let answer = sqs(
        router,
        case,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    answer["Messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The message bodies the queue at `url` holds once at least one is there — or a failure
/// naming the row once [`DEADLINE`] has passed.
pub(crate) async fn delivered_messages(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let bodies = received(router, case, url).await;
        if !bodies.is_empty() {
            return bodies;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the queue leg's message on {url}, got none"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until the stream `name` reports [`ACTIVE`], which is when a record may be put into
/// it — rather than waiting a span the emulator chose, which is not a condition a client can
/// read.
pub(crate) async fn given_an_active_stream(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = firehose(
            router,
            case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": name }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == ACTIVE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stream {name:?} must reach {ACTIVE}, last reported \
             {status:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `document` must be the envelope this suite published, read by MEMBER rather than as text:
/// EventBridge hands a target the whole envelope, whose `source`, `detail-type` and `detail`
/// are the publication's own (`PutEventsRequestEntry`).
pub(crate) fn assert_carries_the_envelope(case: &str, leg: &str, document: &str) {
    let envelope: Value = serde_json::from_str(document).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the {leg} leg receives one JSON envelope, got {document:?} \
             ({error})"
        )
    });
    // (member, what the publication submitted for it)
    let expected = [
        ("source", json!(SOURCE)),
        ("detail-type", json!(DETAIL_TYPE)),
        ("detail", json!({ "orderId": ORDER_ID })),
    ];
    for (member, value) in expected {
        assert_eq!(
            envelope[member], value,
            "case {case:?}: the {leg} leg's envelope carries {member:?} as published; \
             expected {value}, got {envelope}"
        );
    }
    assert!(
        envelope["id"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: the {leg} leg's envelope carries the event's own id, got {envelope}"
    );
}

/// The emulator the suite drives: the five services the fan-out spans, on ONE engine.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A rule carrying a delivery-stream target and a queue target delivers the matching event to
/// BOTH of them, and each leg's own authorization decides that leg alone.
#[tokio::test]
async fn a_rule_fans_one_event_out_to_its_firehose_and_queue_targets() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let rows = rows();

    // The bucket every stream delivers into, and the role every stream delivers under.
    let (status, body) = send(
        &router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "setup: the destination bucket must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let delivery_role = given_role(
        &router,
        "setup",
        DELIVERY_ROLE,
        "firehose.amazonaws.com",
        Some("s3:PutObject"),
    )
    .await;

    // Every row's stream, created together so one wait covers the span the creates armed.
    for row in &rows {
        firehose(
            &router,
            row.case,
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": row.name,
                "DeliveryStreamType": "DirectPut",
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": delivery_role,
                    "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                    "Prefix": format!("{}/", row.name),
                    // The shortest buffering the hints admit, so a row waits for the
                    // emulator's own delivery span and nothing longer.
                    "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                },
            }),
        )
        .await;
    }

    // Every row's rule, carrying BOTH targets: the stream under this row's role, and this
    // row's queue.
    let mut queue_urls: Vec<String> = Vec::new();
    for row in &rows {
        let case = row.case;
        let role_arn = given_role(
            &router,
            case,
            row.name,
            EVENTS_PRINCIPAL,
            Some(row.stream_role_allows),
        )
        .await;
        let (queue_url, queue_arn) =
            given_queue(&router, case, row.name, row.queue_policy_grants).await;
        queue_urls.push(queue_url);
        events(
            &router,
            case,
            "PutRule",
            json!({
                "Name": row.name,
                "EventPattern": json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let stream_target = json!({
            "Id": "stream",
            "Arn": format!("arn:aws:firehose:{REGION}:{account}:deliverystream/{}", row.name),
            "RoleArn": role_arn,
        });
        let queue_target = json!({ "Id": "queue", "Arn": queue_arn });
        let targets = match row.order {
            Order::StreamFirst => json!([stream_target, queue_target]),
            Order::QueueFirst => json!([queue_target, stream_target]),
        };
        let answered = events(
            &router,
            case,
            "PutTargets",
            json!({ "Rule": row.name, "Targets": targets }),
        )
        .await;
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case {case:?}: both targets must be accepted, got {answered}"
        );
    }

    // Firehose refuses a record put at a stream that is still being created, so the
    // publication waits for every stream to REPORT that it is active.
    for row in &rows {
        given_an_active_stream(&router, row.case, row.name).await;
    }

    // ONE publication, which every row's rule matches: what each row's two legs admit is the
    // only thing that differs between them.
    let published = events(
        &router,
        "the publication",
        "PutEvents",
        json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "orderId": ORDER_ID }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "the publication must be accepted whole: {published}"
    );

    // The legs that DELIVER first: a delivered object is the barrier the absences below are
    // judged against, so an empty prefix is an absence rather than a delivery still in flight.
    for row in rows.iter().filter(|row| row.expect_object) {
        let body = delivered_object(&router, row.case, BUCKET, &format!("{}/", row.name)).await;
        assert_carries_the_envelope(row.case, "stream", &body);
    }
    for (row, url) in rows.iter().zip(&queue_urls).filter(|(row, _)| row.expect_message) {
        let bodies = delivered_messages(&router, row.case, url).await;
        assert_eq!(
            bodies.len(),
            1,
            "case {:?}: the queue leg receives the event once, got {bodies:?}",
            row.case
        );
        assert_carries_the_envelope(row.case, "queue", &bodies[0]);
    }

    // And the legs whose own authorization withholds the call deliver nothing — while the
    // other leg of the SAME rule delivered above.
    for row in rows.iter().filter(|row| !row.expect_object) {
        let keys = keys_under(&router, row.case, BUCKET, &format!("{}/", row.name)).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a stream leg the target's role bars reaches no stream, so its \
             destination holds nothing; got {keys:?}",
            row.case
        );
    }
    for (row, url) in rows.iter().zip(&queue_urls).filter(|(row, _)| !row.expect_message) {
        let bodies = received(&router, row.case, url).await;
        assert!(
            bodies.is_empty(),
            "case {:?}: a queue leg the queue's own policy bars receives nothing; got \
             {bodies:?}",
            row.case
        );
    }
}
