// SPDX-License-Identifier: CC0-1.0

//! A Firehose flush carries every record the stream accepted since the previous one —
//! including the records that arrived while that buffer's interval was already running.
//!
//! The rule under test: `BufferingHints$IntervalInSeconds` is "buffer incoming data for the
//! specified period of time, in seconds, before delivering it to the destination" and
//! `CreateDeliveryStream` states the pair as "Firehose buffers data up to 5 MB or for 5
//! minutes, whichever condition is satisfied first" — an UPPER bound on the wait, and the
//! property of the STREAM's flush cadence rather than a delay each record is held for. So
//! the interval is not re-armed by an arrival: a record put into a buffer that is already
//! filling waits what is LEFT of the running interval and rides the flush that buffer is
//! already waiting for, and that one flush writes every record accepted since the previous
//! flush, in arrival order, as one object.
//!
//! What a row observes is CONTENT, never the clock: it puts a uniquely marked record and
//! lists the destination prefix in the same loop, so a listing that answers only the keys
//! already known is an observation that every record put so far is still buffered. When a
//! new key appears, all of those records have to be in one of the objects that flush wrote,
//! in the order they were put — a record held back for an interval of its own would be
//! missing from it. Nothing here asserts on elapsed time, so a loaded host changes only how
//! many records a row gets to put.
//!
//! Only a cross-service suite can pin this: the flush is a Firehose decision and the object
//! it writes is read back out of the Amazon S3 service, with the delivery made under an IAM
//! role — three services over ONE engine, exactly as `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over: the configured interval, how fast
//! the producer puts records into the filling buffer, how large each record is, and which
//! destination configuration (plain or extended, with and without a `Prefix`) carries the
//! hints.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::events_rule_firehose_target_delivery::{given_role, send, REGION};
use super::query_wire::elements;

/// The bucket every row's stream delivers into.
const BUCKET: &str = "firehose-flush-window";

/// The role every stream delivers under: Firehose has to be able to assume it, and it has to
/// allow the object write the delivery makes.
const DELIVERY_ROLE: &str = "firehose-flush-window-role";

/// How long a row waits for its first delivered object, and for the rest of its records to
/// follow: many times any row's own interval, so a loaded host never fails a row the clock
/// would have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);

/// How long a row polls for before giving the stream another look while waiting for the
/// records it has already put.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One row: the hints a stream carries, how its producer feeds it, and how its destination
/// is configured.
struct Row {
    case: &'static str,
    /// Names this row's stream and its destination prefix, so rows cannot collide.
    name: &'static str,
    /// `BufferingHints$IntervalInSeconds` for this row's destination.
    interval_seconds: u64,
    /// How long the producer waits between puts while the buffer fills.
    spacing: Duration,
    /// How many bytes each of this row's records carries.
    record_bytes: usize,
    /// Whether the hints are carried by an `ExtendedS3DestinationConfiguration` (with a
    /// `Prefix` of its own) rather than by a plain `S3DestinationConfiguration`.
    extended: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a fast trickle of small records into a plain destination",
            name: "trickle-plain",
            interval_seconds: 5,
            spacing: Duration::from_millis(150),
            record_bytes: 24,
            extended: false,
        },
        Row {
            case: "a slower trickle of larger records into an extended destination",
            name: "trickle-extended",
            interval_seconds: 9,
            spacing: Duration::from_millis(400),
            record_bytes: 1_024,
            extended: true,
        },
    ]
}

/// The prefix a row's objects are written under, which is also the prefix its listings scope
/// to.
fn prefix_of(row: &Row) -> String {
    format!("{}/", row.name)
}

/// One `awsJson1_1` Firehose or S3 operation that must succeed.
///
/// `pub(crate)` with the two destination reads below, so the sibling suite driving the same
/// rule through an EventBridge rule's Firehose target
/// (`events_rule_firehose_trickle_rides_the_pending_flush`) sends its calls and reads its
/// objects exactly as this one does rather than keeping another copy of them.
pub(crate) async fn json_call(
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

/// Every key `bucket` holds under `prefix`, in key order.
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

/// The bytes `bucket` holds at `key`, read through `GetObject`.
pub(crate) async fn body_of(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
) -> String {
    let (status, body) =
        send(router, "s3", "GET", &format!("/{bucket}/{key}"), &[], Vec::new()).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: reading {key:?} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    String::from_utf8_lossy(&body).to_string()
}

/// The body of each object the bucket holds under `prefix`, in key order.
async fn objects_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let mut bodies = Vec::new();
    for key in keys_under(router, case, BUCKET, prefix).await {
        bodies.push(body_of(router, case, BUCKET, &key).await);
    }
    bodies
}

/// This row's stream, created with its own hints and destination form, once it is `ACTIVE` —
/// a stream still `CREATING` takes no record.
async fn given_active_stream(router: &MultiServiceRouter, row: &Row, role_arn: &str) {
    let destination = json!({
        "RoleARN": role_arn,
        "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
        "Prefix": prefix_of(row),
        // A size threshold far above anything a row puts, so the INTERVAL is what decides
        // every flush below.
        "BufferingHints": { "SizeInMBs": 64, "IntervalInSeconds": row.interval_seconds },
    });
    let mut request = json!({
        "DeliveryStreamName": row.name,
        "DeliveryStreamType": "DirectPut",
    });
    if row.extended {
        request["ExtendedS3DestinationConfiguration"] = destination;
    } else {
        request["S3DestinationConfiguration"] = destination;
    }
    json_call(
        router,
        row.case,
        "firehose",
        "Firehose_20150804.CreateDeliveryStream",
        request,
    )
    .await;

    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = json_call(
            router,
            row.case,
            "firehose",
            "Firehose_20150804.DescribeDeliveryStream",
            json!({ "DeliveryStreamName": row.name }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].clone();
        if status == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the stream must reach ACTIVE, got {status}",
            row.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The marker the `n`th record of a row carries, delimited at both ends so one marker is
/// never a substring of another.
fn marker(row: &Row, n: usize) -> String {
    format!("<{}-{n}>", row.name)
}

#[tokio::test]
async fn one_flush_carries_every_record_buffered_since_the_last_one() {
    let router = {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
        );
        router
    };

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
    let role_arn = given_role(
        &router,
        "setup",
        DELIVERY_ROLE,
        "firehose.amazonaws.com",
        Some("s3:PutObject"),
    )
    .await;

    for row in rows() {
        let case = row.case;
        let prefix = prefix_of(&row);
        given_active_stream(&router, &row, &role_arn).await;

        // Put uniquely marked records into the filling buffer, listing the destination in the
        // same loop: a listing that answers the keys already KNOWN is an observation that
        // every record put so far is still buffered, so when a new key appears, that flush
        // has to carry all of them.
        let mut known: Vec<String> = Vec::new();
        let mut put = 0_usize;
        let mut buffered_from = 0_usize;
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        let (fresh, buffered_to) = loop {
            let text = marker(&row, put);
            let data = format!("{text}{}\n", "p".repeat(row.record_bytes));
            json_call(
                &router,
                case,
                "firehose",
                "Firehose_20150804.PutRecord",
                json!({
                    "DeliveryStreamName": row.name,
                    "Record": { "Data": ::aws_smithy_types::base64::encode(data.as_bytes()) },
                }),
            )
            .await;
            put += 1;
            let keys = keys_under(&router, case, BUCKET, &prefix).await;
            if keys != known {
                let fresh: Vec<String> = keys
                    .iter()
                    .filter(|key| !known.contains(key))
                    .cloned()
                    .collect();
                // The record just put may have arrived after the flush instant, so it is the
                // one record this row does not judge.
                if put - 1 > buffered_from {
                    break (fresh, put - 1);
                }
                // The cadence turned before any record was observed buffered (a row whose
                // stream took longer to settle than its own interval): take the new keys as
                // known and keep feeding the next buffer.
                known = keys;
                buffered_from = put;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the stream's buffer must be flushed to {prefix:?}; {put} \
                 record(s) put and the prefix still holds {known:?}"
            );
            tokio::time::sleep(row.spacing).await;
        };

        // Every record that was observed buffered is in ONE of the objects that flush wrote,
        // in the order it was put: the flush carries what arrived while its interval ran.
        let mut delivered = Vec::new();
        for key in &fresh {
            delivered.push(body_of(&router, case, BUCKET, key).await);
        }
        let first = marker(&row, buffered_from);
        let holder = delivered
            .iter()
            .find(|body| body.contains(&first))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the object(s) {fresh:?} the flush wrote to carry \
                     {first:?}, got {delivered:?}"
                )
            });
        let mut previous = 0_usize;
        for n in buffered_from..buffered_to {
            let text = marker(&row, n);
            let at = holder.find(&text).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: record {n} was buffered before the flush, so the flushed \
                     object carries it; expected {text:?} in an object of {} byte(s)",
                    holder.len()
                )
            });
            assert!(
                n == buffered_from || at > previous,
                "case {case:?}: expected record {n} after record {} in arrival order, got it \
                 at byte {at} against {previous}",
                n - 1
            );
            previous = at;
        }
        assert_eq!(
            holder.matches(&first).count(),
            1,
            "case {case:?}: a flushed record is written once; {first:?} appears more than \
             once in an object of {} byte(s)",
            holder.len()
        );

        // And nothing a put accepted is lost or written twice: the records put after that
        // flush follow it in a later object, each exactly once across the whole prefix.
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let bodies = objects_under(&router, case, &prefix).await;
            let whole = bodies.concat();
            let missing: Vec<String> = (0..put)
                .map(|n| marker(&row, n))
                .filter(|text| !whole.contains(text))
                .collect();
            if missing.is_empty() {
                for n in 0..put {
                    let text = marker(&row, n);
                    let written = whole.matches(&text).count();
                    assert_eq!(
                        written, 1,
                        "case {case:?}: every accepted record is delivered once; {text:?} was \
                         delivered {written} time(s)"
                    );
                }
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: every record a put accepted is delivered within the stream's \
                 buffering interval; {missing:?} of {put} never arrived"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
