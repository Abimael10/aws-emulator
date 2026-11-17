// SPDX-License-Identifier: CC0-1.0

//! A TRICKLE of events published onto an event bus, each matched by a rule whose target is a
//! Firehose delivery stream, reaches the stream's Amazon S3 destination — every event
//! exactly once, none of them left behind in a buffer.
//!
//! The rule under test: for each target of a rule whose `EventPattern` matched, EventBridge
//! makes the call that target's service is invoked by — `firehose:PutRecord` for a delivery
//! stream — acting as the target's `RoleArn`, and the record then follows the stream's own
//! buffering contract: `BufferingHints$IntervalInSeconds` is the longest the stream holds
//! data "before delivering it to the destination", so every record the stream accepted
//! reaches the destination on the next turn of that cadence. A steady producer therefore
//! sees each of its events in the bucket, exactly once: a record is neither dropped when the
//! buffer it joined was already filling nor written twice when the next buffer is flushed.
//!
//! `events_rule_firehose_target_delivery.rs` pins ONE publication and what each target role
//! admits of it; this suite pins what a SEQUENCE of publications into one rule leaves in the
//! destination, which is where a flush that forgets the arrivals of its own interval, or
//! repeats them, shows. The rows differ along what the rule generalises over: how many
//! events are published and how fast, and the detail each one carries.
//!
//! Three services over ONE engine — EventBridge for the rule and the publications, Firehose
//! for the stream, Amazon S3 for the objects, IAM for the two roles — exactly as
//! `core/router/src/lib.rs` mounts them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::events_rule_firehose_target_delivery::{given_role, send, REGION};
use super::query_wire::elements;

/// The bucket the stream below delivers into.
const BUCKET: &str = "events-firehose-trickle";

/// The source the rule matches on, and the detail type every publication carries.
const SOURCE: &str = "com.demo.trickle";
const DETAIL_TYPE: &str = "Order Placed";

/// `BufferingHints$IntervalInSeconds` for the stream: short, so the suite waits for the
/// stream's own cadence rather than for the modelled default.
const INTERVAL_SECONDS: u64 = 3;

/// How many events are published, and how long the producer waits between them.
const PUBLICATIONS: usize = 6;
const SPACING: Duration = Duration::from_millis(250);

/// How long the destination is polled for before the suite fails: many times the stream's own
/// interval, so a loaded host does not fail a delivery the clock would have made.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);

/// How often the destination is polled while waiting for the deliveries.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One `awsJson1_1` operation that must succeed.
async fn json_call(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target: &str,
    body: Value,
) -> Value {
    let (status, answered) = send(
        router,
        signing_name,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
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

/// Everything the bucket holds, concatenated in key order.
async fn delivered(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{BUCKET}?list-type=2"),
        &[],
        Vec::new(),
    )
    .await;
    let listing = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing the destination must succeed, got {status} {listing}"
    );
    let mut keys = elements(&listing, "Key");
    keys.sort();
    let mut whole = String::new();
    for key in keys {
        let (status, body) =
            send(router, "s3", "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new()).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: reading {key:?} must succeed, got {status} {}",
            String::from_utf8_lossy(&body)
        );
        whole.push_str(&String::from_utf8_lossy(&body));
    }
    whole
}

/// The marker the `n`th publication carries, delimited at both ends so one marker is never a
/// substring of another.
fn marker(n: usize) -> String {
    format!("trickle-{n}-end")
}

#[tokio::test]
async fn every_event_of_a_trickle_reaches_the_firehose_targets_destination_once() {
    let case = "a trickle of publications into one Firehose target";
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
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    let account = engine::emulator_account_id();

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
        case,
        "firehose-trickle-delivery",
        "firehose.amazonaws.com",
        Some("s3:PutObject"),
    )
    .await;
    json_call(
        &router,
        case,
        "firehose",
        "Firehose_20150804.CreateDeliveryStream",
        json!({
            "DeliveryStreamName": "trickle",
            "DeliveryStreamType": "DirectPut",
            "ExtendedS3DestinationConfiguration": {
                "RoleARN": delivery_role,
                "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                // A size threshold no publication comes near, so the interval is what
                // decides every flush here.
                "BufferingHints": { "SizeInMBs": 64, "IntervalInSeconds": INTERVAL_SECONDS },
            },
        }),
    )
    .await;
    let settled = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = json_call(
            &router,
            case,
            "firehose",
            "Firehose_20150804.DescribeDeliveryStream",
            json!({ "DeliveryStreamName": "trickle" }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].clone();
        if status == json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < settled,
            "case {case:?}: the stream must reach ACTIVE, got {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // The rule, and its one target: the stream, reached as a role this service may assume
    // and whose policy allows the record write.
    let target_role = given_role(
        &router,
        case,
        "firehose-trickle-target",
        "events.amazonaws.com",
        Some("firehose:PutRecord"),
    )
    .await;
    json_call(
        &router,
        case,
        "events",
        "AWSEvents.PutRule",
        json!({
            "Name": "trickle",
            "EventPattern": json!({ "source": [SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let answered = json_call(
        &router,
        case,
        "events",
        "AWSEvents.PutTargets",
        json!({
            "Rule": "trickle",
            "Targets": [{
                "Id": "stream",
                "Arn": format!("arn:aws:firehose:{REGION}:{account}:deliverystream/trickle"),
                "RoleArn": target_role,
            }],
        }),
    )
    .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {answered}"
    );

    // The trickle: one publication at a time, each carrying its own marker.
    for n in 0..PUBLICATIONS {
        let published = json_call(
            &router,
            case,
            "events",
            "AWSEvents.PutEvents",
            json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "marker": marker(n) }).to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            published["FailedEntryCount"],
            json!(0),
            "case {case:?}: publication {n} must be accepted: {published}"
        );
        tokio::time::sleep(SPACING).await;
    }

    // Every published event reaches the destination, and each of them once: the flush that
    // ends a buffer's interval carries the records that arrived during it, and the next one
    // does not carry them again.
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let whole = delivered(&router, case).await;
        let missing: Vec<String> = (0..PUBLICATIONS)
            .map(marker)
            .filter(|text| !whole.contains(text))
            .collect();
        if missing.is_empty() {
            for n in 0..PUBLICATIONS {
                let text = marker(n);
                let written = whole.matches(&text).count();
                assert_eq!(
                    written, 1,
                    "case {case:?}: every delivered event is written once; {text:?} was \
                     delivered {written} time(s)"
                );
            }
            // What the stream received is what the target received: the whole envelope.
            for expected in [SOURCE, DETAIL_TYPE, "\"detail-type\""] {
                assert!(
                    whole.contains(expected),
                    "case {case:?}: the delivered objects carry the published envelope, \
                     expected {expected:?} in {whole:?}"
                );
            }
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every published event reaches the stream's destination; \
             {missing:?} of {PUBLICATIONS} never arrived"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
