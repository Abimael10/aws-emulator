// SPDX-License-Identifier: CC0-1.0

//! The `AWS/Firehose` series a delivery stream publishes about its own data plane, read where a
//! client reads them: Firehose, Amazon S3, IAM and CloudWatch on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! RULE: a Firehose delivery stream publishes its own data plane into the `AWS/Firehose`
//! namespace, dimensioned by `DeliveryStreamName` — `IncomingRecords` (Count) and
//! `IncomingBytes` (Bytes) for what a put ACCEPTED, and `DeliveryToS3.Records` (Count),
//! `DeliveryToS3.Bytes` (Bytes), `DeliveryToS3.Success` (Count) and
//! `DeliveryToS3.DataFreshness` (Seconds) for what it WROTE to its Amazon S3 destination. The
//! two ends are metered separately, so a put of N records totalling B bytes that lands in the
//! bucket comes to `IncomingRecords` N, `IncomingBytes` B, `DeliveryToS3.Records` N and
//! `DeliveryToS3.Bytes` B, while a stream whose destination role may not write the object
//! accepted the same records — its ingestion counters move and its `DeliveryToS3.*` stay empty.
//!
//! Only a cross-service suite can ask this: the counters are published into CloudWatch's own
//! metric store (the one `ListMetrics` and `GetMetricStatistics` read), the objects land in
//! Amazon S3's, and whether the destination role may write them is IAM's answer. A crate-local
//! test only ever sees one side of it.
//!
//! The rows of the ingestion table differ along what the rule generalises over: how many records
//! one put carries, how large each of them is, and which ingestion operation put them
//! (`PutRecord`, `PutRecordBatch`).
//!
//! Nothing here waits a fixed span: every wait polls for the CONDITION it is about — the objects
//! arriving, the datapoints arriving — and ends as soon as it holds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// CloudWatch's `X-Amz-Target` prefix and the signing name its credential scope carries.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace a delivery stream's own metrics live in, and the dimension naming the stream.
const NAMESPACE: &str = "AWS/Firehose";
const DELIVERY_STREAM_NAME_DIMENSION: &str = "DeliveryStreamName";

/// The series this suite reads.
const INCOMING_RECORDS: &str = "IncomingRecords";
const INCOMING_BYTES: &str = "IncomingBytes";
const DELIVERY_TO_S3_RECORDS: &str = "DeliveryToS3.Records";
const DELIVERY_TO_S3_BYTES: &str = "DeliveryToS3.Bytes";
const DELIVERY_TO_S3_SUCCESS: &str = "DeliveryToS3.Success";
const DELIVERY_TO_S3_DATA_FRESHNESS: &str = "DeliveryToS3.DataFreshness";

/// The status a delivery stream takes records in (`DeliveryStreamStatus`).
const ACTIVE: &str = "ACTIVE";

/// How long a CONDITION is waited for before a case fails: far longer than the spans the
/// pipeline's own clocks need, so a loaded machine does not fail a case the clock would have
/// served.
const DEADLINE: Duration = Duration::from_secs(120);

/// How often a condition is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The one-minute period a metric read windows by (`MetricStat$Period`).
const PERIOD_SECS: f64 = 60.0;

/// How far back and ahead a read looks, in periods: enough to hold every observation a case
/// makes whichever minute it straddles.
const READ_PERIODS_BACK: f64 = 10.0;
const READ_PERIODS_AHEAD: f64 = 2.0;

/// The role whose permissions ALLOW the object write a delivery makes.
const PERMITTED_ROLE: &str = "firehose-permitted";

/// The role whose permissions DENY it.
const DENIED_ROLE: &str = "firehose-denied";

/// The trust policy both roles carry: Firehose assumes the role its destination names.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The permitted role's permissions: the object write a delivery makes.
const ALLOW_PUT_OBJECT: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:PutObject","#,
    r#""Resource":"*"}]}"#
);

/// The denied role's: the same write, refused.
const DENY_PUT_OBJECT: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Action":"s3:PutObject","#,
    r#""Resource":"*"}]}"#
);

/// Firehose, Amazon S3, IAM and CloudWatch on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's concern, not any of these services'.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the routing key
/// the router claims a request by. Nothing here is a credential: this router runs with request
/// authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name` serves; answers its status and body bytes.
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
    let status = response.status().as_u16();
    (status, response.body().to_vec())
}

/// One Firehose operation over its awsJson1_1 wire, requiring a 200.
async fn firehose(router: &MultiServiceRouter, operation: &str, request: &Value) -> Value {
    let (status, answered) = send(
        router,
        "firehose",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Firehose_20150804.{operation}")),
        ],
        request.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "expected HTTP 200 from {operation}, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One CloudWatch operation over its awsJson1_0 wire, requiring a 200.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, request: &Value) -> Value {
    let (status, answered) = send(
        router,
        CLOUDWATCH_SIGNING_NAME,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            (
                "x-amz-target",
                &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
            ),
        ],
        request.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "expected HTTP 200 from {operation}, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One IAM action over its awsQuery wire, requiring a 200.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
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
        "expected HTTP 200 from {action}, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}

/// `PUT /<bucket>` — S3's `CreateBucket`, in the Region every call here is signed for.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let (status, answered) = send(router, "s3", "PUT", &format!("/{bucket}"), &[], Vec::new()).await;
    assert_eq!(
        status,
        200,
        "create bucket {bucket}: got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}

/// Every key `bucket` holds, read through `ListObjectsV2`.
async fn keys(router: &MultiServiceRouter, bucket: &str) -> Vec<String> {
    let (status, answered) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}?list-type=2"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(status, 200, "list {bucket}: got {status} with {text}");
    let mut found = elements(&text, "Key");
    found.sort();
    found
}

/// Create the role `name` with `permissions` inline and the Firehose trust policy.
async fn given_role(router: &MultiServiceRouter, name: &str, permissions: &str) {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "delivery"),
            ("PolicyDocument", permissions),
        ],
    )
    .await;
}

/// The role ARN of this account's role `name`.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// A `DirectPut` stream writing into `bucket` under `role`, waited until it takes records.
async fn given_stream(router: &MultiServiceRouter, stream: &str, bucket: &str, role: &str) {
    create_bucket(router, bucket).await;
    firehose(
        router,
        "CreateDeliveryStream",
        &json!({
            "DeliveryStreamName": stream,
            "ExtendedS3DestinationConfiguration": {
                "RoleARN": role_arn(role),
                "BucketARN": format!("arn:aws:s3:::{bucket}"),
                // The shortest wait `IntervalInSeconds` admits, so a case does not wait out a
                // buffering interval it is not about.
                "BufferingHints": {"SizeInMBs": 1, "IntervalInSeconds": 0},
            },
        }),
    )
    .await;
}

/// Wait until `stream` is `ACTIVE`, which is when it takes records.
async fn until_active(router: &MultiServiceRouter, stream: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = firehose(
            router,
            "DescribeDeliveryStream",
            &json!({"DeliveryStreamName": stream}),
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
            "delivery stream {stream} must reach {ACTIVE} before it takes records, last status \
             {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The instant the current one-minute period opens at.
fn period_start() -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64());
    (now / PERIOD_SECS).floor() * PERIOD_SECS
}

/// The datapoints one `AWS/Firehose` series of `stream` holds over the window around now, read
/// through `GetMetricStatistics` exactly as a client reads them.
async fn datapoints(router: &MultiServiceRouter, metric_name: &str, stream: &str) -> Vec<Value> {
    let answer = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric_name,
            "Dimensions": [{"Name": DELIVERY_STREAM_NAME_DIMENSION, "Value": stream}],
            "StartTime": period_start() - READ_PERIODS_BACK * PERIOD_SECS,
            "EndTime": period_start() + READ_PERIODS_AHEAD * PERIOD_SECS,
            "Period": PERIOD_SECS as i64,
            "Statistics": ["Sum", "Maximum"],
        }),
    )
    .await;
    answer["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The `Sum` of every datapoint one series holds.
async fn series_sum(router: &MultiServiceRouter, metric_name: &str, stream: &str) -> f64 {
    datapoints(router, metric_name, stream)
        .await
        .iter()
        .filter_map(|datapoint| datapoint.get("Sum").and_then(Value::as_f64))
        .sum()
}

/// Wait until one series of `stream` sums to at least `want`, and answer the sum it reached.
async fn until_sum(
    router: &MultiServiceRouter,
    case: &str,
    metric_name: &str,
    stream: &str,
    want: f64,
) -> f64 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let sum = series_sum(router, metric_name, stream).await;
        if sum >= want {
            return sum;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {metric_name} of {stream} to sum to at least {want}, got \
             {sum}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `bucket` holds at least one object.
async fn until_delivered(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let found = keys(router, bucket).await;
        if !found.is_empty() {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivered object in {bucket}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The records of one put, as `Records` entries carrying base64 of `payload` repeated.
fn records(payloads: &[String]) -> Value {
    Value::Array(
        payloads
            .iter()
            .map(|payload| json!({"Data": base64(payload.as_bytes())}))
            .collect(),
    )
}

/// One payload as the wire carries a Firehose record's `Data`: base64, through the maintained
/// codec the rest of this test crate decodes blobs with.
fn base64(bytes: &[u8]) -> String {
    aws_smithy_types::base64::encode(bytes)
}

/// One ingestion row: the records one put carries and which operation puts them.
struct Row {
    case: &'static str,
    /// The stream and bucket the row writes its own resources under.
    name: &'static str,
    /// The payloads the put carries, one per record.
    payloads: Vec<String>,
    /// Whether the put is a `PutRecordBatch` (several records) or a `PutRecord` (one).
    batch: bool,
}

/// The ingestion table.
///
/// The rows differ along what the rule generalises over: how many records one put carries, how
/// large each record is (equal sizes, unequal sizes, a record carrying no data at all), and which
/// ingestion operation put them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one record through PutRecord",
            name: "single",
            payloads: vec!["a".repeat(45)],
            batch: false,
        },
        Row {
            case: "seven records of equal size through PutRecordBatch",
            name: "seven",
            payloads: (0..7).map(|_| "b".repeat(45)).collect(),
            batch: true,
        },
        Row {
            case: "three records of different sizes through PutRecordBatch",
            name: "uneven",
            payloads: vec!["c".to_string(), "c".repeat(20), "c".repeat(300)],
            batch: true,
        },
        Row {
            case: "one record through PutRecordBatch",
            name: "batch-of-one",
            payloads: vec!["d".repeat(7)],
            batch: true,
        },
    ]
}

/// RULE: a put's accepted records and their bytes are what the ingestion counters report, and
/// what the delivery wrote is what the `DeliveryToS3.*` counters report — the same figures, in
/// the documented units, for every shape of put.
#[tokio::test]
async fn a_put_that_lands_in_the_bucket_meters_both_ends_of_the_stream() {
    let router = emulator();
    given_role(&router, PERMITTED_ROLE, ALLOW_PUT_OBJECT).await;
    let rows = rows();
    for row in &rows {
        given_stream(
            &router,
            row.name,
            &format!("{}-sink", row.name),
            PERMITTED_ROLE,
        )
        .await;
    }
    for row in &rows {
        until_active(&router, row.name).await;
    }

    for row in &rows {
        let case = row.case;
        let expected_records = row.payloads.len() as f64;
        let expected_bytes: f64 = row.payloads.iter().map(|payload| payload.len()).sum::<usize>()
            as f64;
        if row.batch {
            let answer = firehose(
                &router,
                "PutRecordBatch",
                &json!({
                    "DeliveryStreamName": row.name,
                    "Records": records(&row.payloads),
                }),
            )
            .await;
            assert_eq!(
                answer["FailedPutCount"],
                json!(0),
                "case {case:?}: every record must be accepted, got {answer}"
            );
        } else {
            firehose(
                &router,
                "PutRecord",
                &json!({
                    "DeliveryStreamName": row.name,
                    "Record": {"Data": base64(row.payloads[0].as_bytes())},
                }),
            )
            .await;
        }

        // What was ACCEPTED, reported at once: the put is what meters it.
        let ingested_records =
            until_sum(&router, case, INCOMING_RECORDS, row.name, expected_records).await;
        let ingested_bytes =
            until_sum(&router, case, INCOMING_BYTES, row.name, expected_bytes).await;
        assert_eq!(
            (ingested_records, ingested_bytes),
            (expected_records, expected_bytes),
            "case {case:?}: expected {INCOMING_RECORDS}={expected_records} and \
             {INCOMING_BYTES}={expected_bytes}, got {ingested_records} and {ingested_bytes}"
        );

        // Then what REACHED the destination, once the buffer has been written.
        let delivered = until_delivered(&router, case, &format!("{}-sink", row.name)).await;
        assert!(
            !delivered.is_empty(),
            "case {case:?}: the put must land in the bucket, got {delivered:?}"
        );
        let written_records =
            until_sum(&router, case, DELIVERY_TO_S3_RECORDS, row.name, expected_records).await;
        let written_bytes =
            until_sum(&router, case, DELIVERY_TO_S3_BYTES, row.name, expected_bytes).await;
        assert_eq!(
            (written_records, written_bytes),
            (expected_records, expected_bytes),
            "case {case:?}: expected {DELIVERY_TO_S3_RECORDS}={expected_records} and \
             {DELIVERY_TO_S3_BYTES}={expected_bytes}, got {written_records} and {written_bytes}"
        );

        // A delivery that happened reports a non-zero maximum on the success series.
        let successes = datapoints(&router, DELIVERY_TO_S3_SUCCESS, row.name).await;
        let success_max = successes
            .iter()
            .filter_map(|datapoint| datapoint.get("Maximum").and_then(Value::as_f64))
            .fold(0.0_f64, f64::max);
        assert!(
            success_max > 0.0,
            "case {case:?}: a delivery that was written must report a {DELIVERY_TO_S3_SUCCESS} \
             maximum above 0, got {success_max} over {successes:?}"
        );

        // And the freshness of the data it wrote, in seconds.
        let freshness = datapoints(&router, DELIVERY_TO_S3_DATA_FRESHNESS, row.name).await;
        assert!(
            !freshness.is_empty(),
            "case {case:?}: a delivery must report its {DELIVERY_TO_S3_DATA_FRESHNESS}, got none"
        );
        for datapoint in &freshness {
            assert_eq!(
                datapoint.get("Unit").and_then(Value::as_str),
                Some("Seconds"),
                "case {case:?}: {DELIVERY_TO_S3_DATA_FRESHNESS} is published in seconds, got \
                 {datapoint}"
            );
        }
    }

    // Each series carries the documented unit, and `ListMetrics` reports every one of them under
    // the dimension naming the stream: the identity a dashboard selects them by.
    for (metric_name, unit) in [
        (INCOMING_RECORDS, "Count"),
        (INCOMING_BYTES, "Bytes"),
        (DELIVERY_TO_S3_RECORDS, "Count"),
        (DELIVERY_TO_S3_BYTES, "Bytes"),
        (DELIVERY_TO_S3_SUCCESS, "Count"),
        (DELIVERY_TO_S3_DATA_FRESHNESS, "Seconds"),
    ] {
        let listed = cloudwatch(
            &router,
            "ListMetrics",
            &json!({"Namespace": NAMESPACE, "MetricName": metric_name}),
        )
        .await;
        let metrics = listed["Metrics"].as_array().cloned().unwrap_or_default();
        let dimensioned: Vec<Vec<String>> = metrics
            .iter()
            .map(|metric| {
                metric["Dimensions"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|dimension| {
                        dimension.get("Name").and_then(Value::as_str).map(str::to_string)
                    })
                    .collect()
            })
            .collect();
        assert!(
            !metrics.is_empty()
                && dimensioned
                    .iter()
                    .all(|names| names == &[DELIVERY_STREAM_NAME_DIMENSION.to_string()]),
            "case {metric_name:?}: every series must be listed under the \
             ({DELIVERY_STREAM_NAME_DIMENSION},) dimension set, got {dimensioned:?}"
        );
        for row in &rows {
            let holds = datapoints(&router, metric_name, row.name).await;
            assert!(
                !holds.is_empty() && holds.iter().all(|datapoint| {
                    datapoint.get("Unit").and_then(Value::as_str) == Some(unit)
                }),
                "case {metric_name:?} of {}: every datapoint must carry the unit {unit}, got \
                 {holds:?}",
                row.name
            );
        }
    }
}

/// RULE: ingestion and delivery are metered separately, so a stream whose destination role may
/// not write the object counts what it ACCEPTED all the same while its `DeliveryToS3.*` series
/// stay empty — which is what tells a healthy stream from a broken one.
#[tokio::test]
async fn a_stream_whose_role_cannot_write_counts_ingestion_and_no_delivery() {
    let router = emulator();
    given_role(&router, PERMITTED_ROLE, ALLOW_PUT_OBJECT).await;
    given_role(&router, DENIED_ROLE, DENY_PUT_OBJECT).await;
    given_stream(&router, "permitted", "permitted-sink", PERMITTED_ROLE).await;
    given_stream(&router, "denied", "denied-sink", DENIED_ROLE).await;
    until_active(&router, "permitted").await;
    until_active(&router, "denied").await;

    // The same put on both streams: seven records totalling the same bytes.
    let payloads: Vec<String> = (0..7).map(|_| "e".repeat(45)).collect();
    let expected_records = payloads.len() as f64;
    let expected_bytes = payloads.iter().map(String::len).sum::<usize>() as f64;
    for stream in ["permitted", "denied"] {
        let answer = firehose(
            &router,
            "PutRecordBatch",
            &json!({"DeliveryStreamName": stream, "Records": records(&payloads)}),
        )
        .await;
        assert_eq!(
            answer["FailedPutCount"],
            json!(0),
            "stream {stream}: every record must be accepted, got {answer}"
        );
    }

    // Both streams accepted the records, so both count the same ingestion.
    for stream in ["permitted", "denied"] {
        let ingested = until_sum(&router, stream, INCOMING_RECORDS, stream, expected_records).await;
        let bytes = until_sum(&router, stream, INCOMING_BYTES, stream, expected_bytes).await;
        assert_eq!(
            (ingested, bytes),
            (expected_records, expected_bytes),
            "stream {stream}: a put the stream accepted is counted whatever its destination role \
             may do, got {INCOMING_RECORDS}={ingested} and {INCOMING_BYTES}={bytes}"
        );
    }

    // The permitted stream's delivery is what says the deliveries of this publish have settled:
    // both streams were put to in the same way, so once this one has written, the denied one has
    // had its chance too.
    until_delivered(&router, "permitted", "permitted-sink").await;
    let written = until_sum(
        &router,
        "permitted",
        DELIVERY_TO_S3_RECORDS,
        "permitted",
        expected_records,
    )
    .await;
    assert_eq!(
        written, expected_records,
        "stream permitted: expected {DELIVERY_TO_S3_RECORDS}={expected_records}, got {written}"
    );

    let parked = keys(&router, "denied-sink").await;
    assert!(
        parked.is_empty(),
        "stream denied: a role that may not write the object delivers nothing, got {parked:?}"
    );
    for metric_name in [
        DELIVERY_TO_S3_RECORDS,
        DELIVERY_TO_S3_BYTES,
        DELIVERY_TO_S3_SUCCESS,
        DELIVERY_TO_S3_DATA_FRESHNESS,
    ] {
        let holds = datapoints(&router, metric_name, "denied").await;
        assert!(
            holds.is_empty(),
            "stream denied: nothing reached Amazon S3, so {metric_name} must hold no datapoint, \
             got {holds:?}"
        );
    }
}
