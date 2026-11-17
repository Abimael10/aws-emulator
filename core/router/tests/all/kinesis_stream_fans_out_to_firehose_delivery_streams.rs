// SPDX-License-Identifier: CC0-1.0

//! One Kinesis data stream feeding SEVERAL Firehose delivery streams: every attached stream
//! delivers the whole record set to its own Amazon S3 destination.
//!
//! The rule under test: a Firehose stream whose `DeliveryStreamType` is `KinesisStreamAsSource`
//! consumes, for ITSELF, the Kinesis data stream its `KinesisStreamSourceConfiguration` names —
//! "the stream and role Amazon Resource Names (ARNs) for a Kinesis data stream used as the
//! source for a Firehose stream", a per-stream binding, each with its own instant retrieval
//! starts at (`KinesisStreamSourceDescription$DeliveryStartTimestamp`) — so how far one stream
//! has read the source never limits another: every record the source holds from that instant on
//! is buffered by EVERY stream reading it and delivered to that stream's own destination, each
//! sink receiving the full record set rather than a partition of it.
//!
//! Only a cross-service suite can pin this on the path a client drives: the flow spans four
//! services mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them — Kinesis Data
//! Streams for the shared source, IAM for the role each delivery stream reads its source and
//! writes its bucket under (Firehose resolves every `RoleARN` against IAM when the stream is
//! written), Firehose for the attached streams, and Amazon S3 for the objects the deliveries
//! land. The Firehose crate's own `kinesis_source_fan_out` suite drives the same rule with no
//! IAM mounted and so cannot show that a fan-out built with real, per-consumer delivery roles
//! delivers at all.
//!
//! The rows differ along what the rule generalises over: how many streams are attached to the
//! one source, whether their destinations are separate buckets or one bucket under their own
//! prefixes, whether a destination names a `Prefix` of its own at all, whether each stream
//! carries its own delivery role, which destination member carries the Amazon S3 location, how
//! many shards the source spreads the records over, and whether the producer writes the record
//! set with one `PutRecord` per record or with one `PutRecords`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode};

/// The Region every call below is signed for, and the one every bucket is created in.
const REGION: &str = "us-west-2";

/// How long a delivery out of the shared source is waited for before a case fails: far longer
/// than the emulator's own `CREATING` -> `ACTIVE` span, source poll and buffer window together,
/// so a loaded machine never fails a case the clock would have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(30);

/// How often a status or a destination is looked at again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long each sink is WATCHED once every record has arrived, by looking again every
/// [`POLL_INTERVAL`]: long enough to cover several source polls and buffer windows, so a record
/// delivered twice — or one a stream should not have retrieved — fails the case.
const WATCH: Duration = Duration::from_millis(1_200);

/// The trust policy a Firehose delivery role carries.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// What the role is allowed: the source read a `KinesisStreamAsSource` stream makes, and the
/// object write its destination makes.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["kinesis:GetRecords","kinesis:GetShardIterator","kinesis:DescribeStream","#,
    r#""s3:PutObject"],"Resource":"*"}]}"#
);

/// One delivery stream attached to the row's source, and where it delivers.
#[derive(Debug)]
struct Consumer {
    /// The `CreateDeliveryStream` member carrying this stream's Amazon S3 destination.
    member: &'static str,
    /// Which sink bucket of the row this stream writes to, by index — two consumers naming the
    /// same index share one bucket.
    bucket: usize,
    /// The `Prefix` the destination is configured with; empty leaves the member out.
    prefix: &'static str,
    /// Which of the row's delivery roles this stream names, by index.
    role: usize,
}

/// One fan-out case: the source's shape, the streams attached to it, and the record set a
/// producer writes to the source.
struct Row {
    case: &'static str,
    /// How many shards the source stream is created with.
    shards: u32,
    /// How many sink buckets the row creates for its consumers to index into.
    buckets: usize,
    /// How many IAM delivery roles the row creates for its consumers to index into.
    roles: usize,
    consumers: Vec<Consumer>,
    /// How many records the producer writes to the source.
    records: usize,
    /// How many bytes of filler each record carries beyond its marker.
    filler: usize,
    /// Whether the record set is written with ONE `PutRecords` (true) or one `PutRecord` per
    /// record (false).
    batch: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "two streams, separate buckets, a role each, one PutRecord per record",
            shards: 1,
            buckets: 2,
            roles: 2,
            consumers: vec![
                Consumer {
                    member: "ExtendedS3DestinationConfiguration",
                    bucket: 0,
                    prefix: "raw/",
                    role: 0,
                },
                Consumer {
                    // A destination naming no `Prefix` of its own, whose objects land under the
                    // automatic time expansion alone.
                    member: "S3DestinationConfiguration",
                    bucket: 1,
                    prefix: "",
                    role: 1,
                },
            ],
            records: 4,
            filler: 0,
            batch: false,
        },
        Row {
            case: "three streams sharing one bucket and one role, three shards, one PutRecords",
            shards: 3,
            buckets: 1,
            roles: 1,
            consumers: vec![
                Consumer {
                    member: "S3DestinationConfiguration",
                    bucket: 0,
                    prefix: "landing/",
                    role: 0,
                },
                Consumer {
                    member: "ExtendedS3DestinationConfiguration",
                    bucket: 0,
                    prefix: "curated/",
                    role: 0,
                },
                Consumer {
                    member: "ExtendedS3DestinationConfiguration",
                    bucket: 0,
                    prefix: "archive/nested/",
                    role: 0,
                },
            ],
            records: 6,
            filler: 32,
            batch: true,
        },
    ]
}

/// The four services this flow spans, on ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
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
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The signature is
/// never checked: this router runs with authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name`; answers its status and body bytes.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One `awsJson1_1` operation of `signing_name`, answering its status and parsed body.
async fn json_rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
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
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Kinesis Data Streams operation that must succeed.
async fn kinesis(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answered) =
        json_rpc(router, "kinesis", &format!("Kinesis_20131202.{operation}"), body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Kinesis {operation} must succeed, got {status} {answered}"
    );
    answered
}

/// One Firehose operation that must succeed.
async fn firehose(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answered) = json_rpc(
        router,
        "firehose",
        &format!("Firehose_20150804.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Firehose {operation} must succeed, got {status} {answered}"
    );
    answered
}

/// One `awsQuery` IAM action that must succeed.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, parameters: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in parameters {
        body.push('&');
        body.push_str(key);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = send(
        router,
        "iam",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: IAM {action} must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// Create the IAM role `name`, trusted by the Firehose service principal and allowed the source
/// read and the object write a delivery makes, and answer its ARN.
async fn given_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", TRUST_POLICY)],
    )
    .await;
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "fan-out-delivery"),
            ("PolicyDocument", ROLE_POLICY),
        ],
    )
    .await;
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// Create the sink bucket `name`.
async fn given_bucket(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{name}"),
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
        "case {case:?}: the sink bucket {name} must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// Create the source Kinesis data stream `name` with `shards` shards and answer the `StreamARN`
/// it reports once it is `ACTIVE`.
///
/// A poll loop on the STATUS, not a wait for the clock-owed span, so how fast the host runs
/// cannot decide a case.
async fn given_source(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    shards: u32,
) -> String {
    kinesis(
        router,
        case,
        "CreateStream",
        json!({ "StreamName": name, "ShardCount": shards }),
    )
    .await;
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = kinesis(
            router,
            case,
            "DescribeStreamSummary",
            json!({ "StreamName": name }),
        )
        .await;
        let summary = &described["StreamDescriptionSummary"];
        if summary["StreamStatus"] == json!("ACTIVE") {
            return summary["StreamARN"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the source {name} reports its ARN, got {described}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the source {name} must reach ACTIVE, got {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Attach the `KinesisStreamAsSource` Firehose stream `name` to `source_arn`, delivering to
/// `bucket` under the consumer's prefix with `role_arn`, and wait for it to report `ACTIVE`
/// reading that source.
async fn attach_consumer(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source_arn: &str,
    consumer: &Consumer,
    bucket: &str,
    role_arn: &str,
) {
    let mut destination = json!({
        "RoleARN": role_arn,
        "BucketARN": format!("arn:aws:s3:::{bucket}"),
        // The smallest size and the shortest interval the member admits, so a case waits for the
        // fan-out and not for a buffer window it did not ask to test.
        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
    });
    if !consumer.prefix.is_empty() {
        destination["Prefix"] = json!(consumer.prefix);
    }
    let mut request = json!({
        "DeliveryStreamName": name,
        "DeliveryStreamType": "KinesisStreamAsSource",
        "KinesisStreamSourceConfiguration": {
            "KinesisStreamARN": source_arn,
            "RoleARN": role_arn,
        },
    });
    request[consumer.member] = destination;
    let answered = firehose(router, case, "CreateDeliveryStream", request).await;
    assert!(
        answered["DeliveryStreamARN"]
            .as_str()
            .is_some_and(|arn| arn.ends_with(&format!("deliverystream/{name}"))),
        "case {case:?}: the create answers {name}'s ARN, got {answered}"
    );

    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = firehose(
            router,
            case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": name }),
        )
        .await;
        let description = &described["DeliveryStreamDescription"];
        if description["DeliveryStreamStatus"] == json!("ACTIVE") {
            let source = &description["Source"]["KinesisStreamSourceDescription"];
            assert_eq!(
                source["KinesisStreamARN"],
                json!(source_arn),
                "case {case:?}: {name} reports the source it reads, got {described}"
            );
            assert!(
                source["DeliveryStartTimestamp"].is_number(),
                "case {case:?}: {name} reports when ITS retrieval starts, got {described}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {name} must reach ACTIVE, got {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Write one record to the source, as a producer does.
async fn put_record(router: &MultiServiceRouter, case: &str, source: &str, index: usize, data: &[u8]) {
    let answered = kinesis(
        router,
        case,
        "PutRecord",
        json!({
            "StreamName": source,
            "PartitionKey": format!("pk-{index}"),
            "Data": ::aws_smithy_types::base64::encode(data),
        }),
    )
    .await;
    assert!(
        answered["SequenceNumber"].is_string(),
        "case {case:?}: the source answers the sequence number it stored the record under, got \
         {answered}"
    );
}

/// Write the whole record set to the source with ONE `PutRecords`, as a batching producer does.
async fn put_records(
    router: &MultiServiceRouter,
    case: &str,
    source: &str,
    payloads: &[Vec<u8>],
) {
    let entries: Vec<Value> = payloads
        .iter()
        .enumerate()
        .map(|(index, payload)| {
            json!({
                "PartitionKey": format!("pk-{index}"),
                "Data": ::aws_smithy_types::base64::encode(payload),
            })
        })
        .collect();
    let answered = kinesis(
        router,
        case,
        "PutRecords",
        json!({ "StreamName": source, "Records": entries }),
    )
    .await;
    assert_eq!(
        answered["FailedRecordCount"].as_i64().unwrap_or(0),
        0,
        "case {case:?}: the source takes every record of the batch, got {answered}"
    );
    assert_eq!(
        answered["Records"].as_array().map_or(0, Vec::len),
        payloads.len(),
        "case {case:?}: the source answers one result per record, got {answered}"
    );
}

/// Every key `bucket` holds under `prefix`, read through `ListObjectsV2`.
async fn keys_under(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        // Every prefix of this suite is made of unreserved characters and `/`, so it travels in
        // the query string as written.
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing {bucket}/{prefix} must succeed, got {status} {text}"
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// The bytes `bucket` holds at `key`.
async fn object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) -> Vec<u8> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}/{key}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: reading {bucket}/{key} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// The bytes of every object under `prefix`, keyed by key.
async fn bodies_under(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> BTreeMap<String, Vec<u8>> {
    let mut bodies = BTreeMap::new();
    for key in keys_under(router, case, bucket, prefix).await {
        let body = object(router, case, bucket, &key).await;
        bodies.insert(key, body);
    }
    bodies
}

/// Every object under `prefix`, once each of `payloads` is in one of them — or a failure naming
/// the case once [`DELIVERY_DEADLINE`] has passed.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
    payloads: &[Vec<u8>],
) -> BTreeMap<String, Vec<u8>> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let bodies = bodies_under(router, case, bucket, prefix).await;
        let held: Vec<u8> = bodies.values().flatten().copied().collect();
        let missing: Vec<usize> = payloads
            .iter()
            .enumerate()
            .filter(|(_, payload)| occurrences(&held, payload) == 0)
            .map(|(index, _)| index)
            .collect();
        if missing.is_empty() {
            return bodies;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every record of the source under {bucket}/{prefix}, missing \
             the records at {missing:?} of {} (the keys delivered are {:?})",
            payloads.len(),
            bodies.keys().collect::<Vec<_>>()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Watch `prefix` over [`WATCH`], failing the moment it stops holding each of `payloads` exactly
/// once and nothing beyond them: another consumer's reading of the shared source neither
/// duplicates a record here nor takes one away.
///
/// A poll loop rather than one look after a wait, so a second delivery fails the case whenever in
/// the window it happens and the window's length decides nothing but how long the watch lasts.
async fn stays_delivered(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
    payloads: &[Vec<u8>],
) {
    let until = Instant::now() + WATCH;
    loop {
        let bodies = bodies_under(router, case, bucket, prefix).await;
        let held: Vec<u8> = bodies.values().flatten().copied().collect();
        for payload in payloads {
            let counted = occurrences(&held, payload);
            assert_eq!(
                counted, 1,
                "case {case:?}: {bucket}/{prefix} holds the record {:?} exactly once, got \
                 {counted}",
                summarised(payload)
            );
        }
        let expected: usize = payloads.iter().map(Vec::len).sum();
        assert_eq!(
            held.len(),
            expected,
            "case {case:?}: {bucket}/{prefix} holds the record set and nothing else, got {} bytes \
             under {:?}",
            held.len(),
            bodies.keys().collect::<Vec<_>>()
        );
        if Instant::now() >= until {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How many times `needle` occurs in `held`.
fn occurrences(held: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || held.len() < needle.len() {
        return 0;
    }
    held.windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// A record shortened for an assertion message: the head carrying its marker, not the filler.
fn summarised(record: &[u8]) -> String {
    let head = record.len().min(40);
    let mut shown = String::from_utf8_lossy(&record[..head]).into_owned();
    if head < record.len() {
        shown.push_str("...");
    }
    shown
}

/// The `index`th record of the `ordinal`th row: a marker no other record of the suite carries,
/// padded with a repeated digit — which cannot hold another record's marker, so counting a marker
/// in the concatenated objects counts that record and nothing else.
fn payload(ordinal: usize, index: usize, filler: usize) -> Vec<u8> {
    let mut payload = format!("fan-out-{ordinal}-record-{index}:").into_bytes();
    payload.extend(std::iter::repeat_n(b'7', filler));
    payload
}

/// Whether `key` carries the automatic `YYYY/MM/DD/HH` expansion Firehose puts after a
/// destination's own `Prefix`.
fn carries_time_prefix(key: &str, prefix: &str) -> bool {
    let Some(rest) = key.strip_prefix(prefix) else {
        return false;
    };
    let fields: Vec<&str> = rest.split('/').take(4).collect();
    fields.len() == 4
        && [4usize, 2, 2, 2]
            .iter()
            .zip(&fields)
            .all(|(width, field)| {
                field.len() == *width && field.bytes().all(|byte| byte.is_ascii_digit())
            })
}

/// The case `case` as observed through the stream `stream`: what an assertion names, since every
/// row drives several streams through the same case.
fn labelled(case: &str, stream: &str) -> String {
    format!("{case} ({stream})")
}

#[tokio::test]
async fn every_delivery_stream_on_one_kinesis_stream_lands_every_record() {
    for (ordinal, row) in rows().into_iter().enumerate() {
        let router = emulator();
        let case = row.case;

        let mut roles = Vec::new();
        for index in 0..row.roles {
            roles.push(given_role(&router, case, &format!("fan-out-role-{ordinal}-{index}")).await);
        }
        let buckets: Vec<String> = (0..row.buckets)
            .map(|index| format!("fan-out-sink-{ordinal}-{index}"))
            .collect();
        for bucket in &buckets {
            given_bucket(&router, case, bucket).await;
        }

        let source = format!("fan-out-source-{ordinal}");
        let source_arn = given_source(&router, case, &source, row.shards).await;

        // Every consumer is attached, and ACTIVE, BEFORE the producer writes, so the whole
        // record set is each one's data.
        let mut streams = Vec::new();
        for (index, consumer) in row.consumers.iter().enumerate() {
            let name = format!("fan-out-stream-{ordinal}-{index}");
            attach_consumer(
                &router,
                case,
                &name,
                &source_arn,
                consumer,
                &buckets[consumer.bucket],
                &roles[consumer.role],
            )
            .await;
            streams.push(name);
        }

        for consumer in &row.consumers {
            let idle = keys_under(
                &router,
                case,
                &buckets[consumer.bucket],
                consumer.prefix,
            )
            .await;
            assert!(
                idle.is_empty(),
                "case {case:?}: an idle source delivers nothing under {:?}, got {idle:?}",
                consumer.prefix
            );
        }

        // What the producer writes to the SOURCE, once — never once per consumer.
        let payloads: Vec<Vec<u8>> = (0..row.records)
            .map(|index| payload(ordinal, index, row.filler))
            .collect();
        if row.batch {
            put_records(&router, case, &source, &payloads).await;
        } else {
            for (index, data) in payloads.iter().enumerate() {
                put_record(&router, case, &source, index, data).await;
            }
        }

        // Each attached stream delivers the WHOLE record set to its own destination.
        for (consumer, stream) in row.consumers.iter().zip(&streams) {
            let bucket = &buckets[consumer.bucket];
            let label = labelled(case, stream);
            let bodies = delivered(&router, &label, bucket, consumer.prefix, &payloads).await;
            for key in bodies.keys() {
                assert!(
                    carries_time_prefix(key, consumer.prefix),
                    "case {label:?}: a delivered key carries {:?} then the automatic \
                     YYYY/MM/DD/HH prefix, got {key:?}",
                    consumer.prefix
                );
                assert!(
                    key.contains(stream),
                    "case {label:?}: an object delivered by {stream} is named for it, got {key:?}"
                );
            }
        }

        // And each delivers it ONCE: another consumer's reading of the shared source neither
        // duplicates a record nor takes one away.
        for (consumer, stream) in row.consumers.iter().zip(&streams) {
            stays_delivered(
                &router,
                &labelled(case, stream),
                &buckets[consumer.bucket],
                consumer.prefix,
                &payloads,
            )
            .await;
        }
    }
}
