// SPDX-License-Identifier: CC0-1.0

//! A Kinesis CHANNEL with a general purpose Amazon S3 destination delivers every record its
//! source stream takes to that bucket, exactly once.
//!
//! The rule under test: a channel "reads records from the specified stream and writes them to
//! ... a general purpose Amazon S3 bucket" (`CreateChannel`), so once the channel is `ACTIVE`
//! every payload a producer writes to its source stream appears in the destination bucket
//! ONCE — under a key rendered from the destination's `OutputKeyTemplate` ("the template used
//! to construct the Amazon S3 object key for delivered objects",
//! `S3StorageConfiguration$OutputKeyTemplate`), holding the record's own bytes, and within the
//! delivery window the destination's `DataFreshnessInSeconds` bounds ("the maximum age, in
//! seconds, of undelivered data before the channel delivers it to the destination"). Nothing a
//! client calls performs the delivery: the channel is a running reader of its stream, so a
//! second batch written later is delivered too and the first batch is not delivered again.
//!
//! Only a cross-service suite can pin this: the flow spans three services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them — Kinesis for the stream and the channel,
//! IAM for the service execution role the channel delivers under, and Amazon S3 for the
//! objects the delivery writes and a reader reads back.
//!
//! The rows differ along what the rule generalises over: how many records one batch carries,
//! the record format the source stream is read in, the size of a payload, and whether the
//! destination names a key template or takes the default one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode};

/// The Region every call below is signed for, and the one every ARN here names.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix of the Kinesis service.
const KINESIS_TARGET_PREFIX: &str = "Kinesis_20131202";

/// The role every channel here delivers under.
const ROLE: &str = "kinesis-channel-delivery";

/// The trust policy that role carries: the channel assumes it, so this service's principal
/// may.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"#,
    r#""kinesis.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// Its permissions: the object write a delivery makes.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["s3:PutObject"],"Resource":"*"}]}"#
);

/// The key template the rows that name one deliver under, and the prefix every key it renders
/// therefore carries.
const TEMPLATE: &str = "deliveries/!{yyyy}/!{MM}/!{dd}/!{channel-name}/!{sequence-number}!{extension}";
const TEMPLATE_PREFIX: &str = "deliveries/";

/// How long a row keeps LOOKING for a condition the emulator reaches on its own clock — a
/// stream that is `ACTIVE`, a channel that has settled, a delivery that has been made. Far
/// longer than any span the emulator chose, so a loaded host only makes a row look again; no
/// row measures a duration.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a row waits between two looks.
const BETWEEN_LOOKS: Duration = Duration::from_millis(50);

/// One row: a source stream, a channel on it, and the batch its producer writes.
struct Row {
    case: &'static str,
    /// Names this row's stream, channel and destination bucket, so rows cannot collide.
    name: &'static str,
    /// "The format of records on the source stream" (`RecordConfiguration$RecordFormatType`).
    record_format: &'static str,
    /// The destination's `OutputKeyTemplate`, or `None` for a destination that names none and
    /// takes the default template.
    template: Option<&'static str>,
    /// The payloads the row's `PutRecords` carries.
    payloads: Vec<String>,
}

/// The payload of record `index` of the row `name` writes: distinct per row and per record, so
/// an assertion names exactly what it did or did not find. `filler` makes a row's payloads as
/// large as it wants them.
fn payload(name: &str, index: usize, filler: usize) -> String {
    format!("{name}-record-{index}-{}", "x".repeat(filler))
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one record, a template naming both expansions",
            name: "onerecord",
            record_format: "STRING",
            template: Some(TEMPLATE),
            payloads: vec![payload("onerecord", 0, 0)],
        },
        Row {
            case: "a batch of records, the default template",
            name: "batched",
            record_format: "JSON",
            template: None,
            payloads: (0..5).map(|index| payload("batched", index, 0)).collect(),
        },
        Row {
            case: "payloads of a few kilobytes each",
            name: "larger",
            record_format: "BYTE_ARRAY",
            template: Some(TEMPLATE),
            payloads: (0..3)
                .map(|index| payload("larger", index, 4_096))
                .collect(),
        },
    ]
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The signature
/// is never checked: this router runs with authentication off.
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

/// One Kinesis operation, answering its status and parsed body.
async fn kinesis(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, answered) = send(
        router,
        "kinesis",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            (
                "x-amz-target",
                &format!("{KINESIS_TARGET_PREFIX}.{operation}"),
            ),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let parsed = serde_json::from_slice(&answered).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Kinesis operation that must succeed.
async fn kinesis_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = kinesis(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} {parsed}"
    );
    parsed
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

/// Every key the bucket `bucket` holds, read through `ListObjectsV2`.
async fn keys_in(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{bucket}?list-type=2"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing {bucket} must succeed, got {status} {text}"
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// The bytes the bucket holds at `key`, read through `GetObject`.
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
        "case {case:?}: reading {key:?} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// Every record every delivered object of `bucket` holds, in key order, beside the keys they
/// were delivered under.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
) -> (Vec<String>, Vec<String>) {
    let keys = keys_in(router, case, bucket).await;
    let mut records = Vec::new();
    for key in &keys {
        let body = object(router, case, bucket, key).await;
        let text = String::from_utf8_lossy(&body).to_string();
        records.extend(
            text.split('\n')
                .filter(|line| !line.is_empty())
                .map(str::to_string),
        );
    }
    (keys, records)
}

/// Look in `bucket` until it holds `expected` records, and fail once [`POLL_DEADLINE`] has
/// passed. Answers the keys and the records found.
async fn poll_until_delivered(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    expected: usize,
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (keys, records) = delivered(router, case, bucket).await;
        if records.len() >= expected {
            return (keys, records);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} delivered records in {bucket}, got \
             {} under the keys {keys:?}",
            records.len()
        );
        tokio::time::sleep(BETWEEN_LOOKS).await;
    }
}

/// Look at `operation` until the resource it reads reports `ACTIVE`.
async fn poll_until_active(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    pointer: &str,
) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, answered) = kinesis(router, operation, body).await;
        if status == 200 && answered.pointer(pointer).and_then(Value::as_str) == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {operation} to report ACTIVE at {pointer}, got {status} \
             {answered}"
        );
        tokio::time::sleep(BETWEEN_LOOKS).await;
    }
}

/// A `PutRecords` batch carrying `payloads`, each under its own partition key: a blob member
/// travels base64-encoded in an awsJson body.
fn batch(stream_arn: &str, payloads: &[String]) -> Value {
    let records: Vec<Value> = payloads
        .iter()
        .enumerate()
        .map(|(index, data)| {
            json!({
                "Data": ::aws_smithy_types::base64::encode(data.as_bytes()),
                "PartitionKey": format!("key-{index}"),
            })
        })
        .collect();
    json!({ "StreamARN": stream_arn, "Records": records })
}

/// The emulator the suite drives: the three services the flow spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

#[tokio::test]
async fn an_s3_channel_delivers_every_put_records_payload_once() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let role_arn = format!("arn:aws:iam::{account}:role/{ROLE}");
    let rows = rows();

    // The role every channel delivers under.
    iam(
        &router,
        "setup",
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", TRUST_POLICY)],
    )
    .await;
    iam(
        &router,
        "setup",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "delivery"),
            ("PolicyDocument", ROLE_POLICY),
        ],
    )
    .await;

    // Each row's destination bucket and source stream, created together so one wait covers
    // the spans the creates armed.
    for row in &rows {
        let (status, body) = send(
            &router,
            "s3",
            "PUT",
            &format!("/{}", row.name),
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
            "case {:?}: the destination bucket must be created, got {status} {}",
            row.case,
            String::from_utf8_lossy(&body)
        );
        kinesis_ok(
            &router,
            row.case,
            "CreateStream",
            &json!({
                "StreamName": row.name,
                // A channel is "only supported for data streams with the on-demand capacity
                // mode" (`CreateChannel`).
                "StreamModeDetails": { "StreamMode": "ON_DEMAND" },
            }),
        )
        .await;
    }

    // Then each row's channel, once its source stream is ready to serve.
    for row in &rows {
        let stream_arn = format!("arn:aws:kinesis:{REGION}:{account}:stream/{}", row.name);
        poll_until_active(
            &router,
            row.case,
            "DescribeStreamSummary",
            &json!({ "StreamARN": stream_arn }),
            "/StreamDescriptionSummary/StreamStatus",
        )
        .await;
        let mut storage = json!({
            "BucketARN": format!("arn:aws:s3:::{}", row.name),
            "ExpectedBucketOwner": account,
            // The compression every row's objects are delivered under: "NONE - No
            // compression" (`S3StorageConfiguration$CompressionType`), so a reader of the
            // object reads the producer's own bytes.
            "CompressionType": "NONE",
        });
        if let Some(template) = row.template {
            storage["OutputKeyTemplate"] = json!(template);
        }
        kinesis_ok(
            &router,
            row.case,
            "CreateChannel",
            &json!({
                "ChannelName": row.name,
                "ServiceExecutionRoleARN": role_arn,
                "StreamConfigurationList": [{
                    "StreamARN": stream_arn,
                    "RecordConfiguration": { "RecordFormatType": row.record_format },
                }],
                "S3DestinationConfiguration": { "StorageConfiguration": storage },
            }),
        )
        .await;
    }

    // Each row's batch, written once every channel is ACTIVE: "After provisioning is
    // complete, Amazon Kinesis Data Streams sets the state to ACTIVE" (`CreateChannel`).
    for row in &rows {
        let channel_arn = format!("arn:aws:kinesis:{REGION}:{account}:channel/{}", row.name);
        poll_until_active(
            &router,
            row.case,
            "DescribeChannel",
            &json!({ "ChannelARN": channel_arn }),
            "/ChannelDescription/ChannelStatus",
        )
        .await;
        let stream_arn = format!("arn:aws:kinesis:{REGION}:{account}:stream/{}", row.name);
        let answered = kinesis_ok(
            &router,
            row.case,
            "PutRecords",
            &batch(&stream_arn, &row.payloads),
        )
        .await;
        assert_eq!(
            answered["FailedRecordCount"].as_i64(),
            Some(0),
            "case {:?}: the whole batch must be taken, got {answered}",
            row.case
        );
    }

    // And what the destination holds: every payload once, under keys the template rendered.
    for row in &rows {
        let case = row.case;
        let (keys, records) =
            poll_until_delivered(&router, case, row.name, row.payloads.len()).await;
        assert_eq!(
            records.len(),
            row.payloads.len(),
            "case {case:?}: expected exactly {} delivered records, got {} under the keys \
             {keys:?}",
            row.payloads.len(),
            records.len()
        );
        for payload in &row.payloads {
            let found = records.iter().filter(|record| *record == payload).count();
            assert_eq!(
                found, 1,
                "case {case:?}: expected {payload:?} delivered exactly once, got {found} \
                 times under the keys {keys:?}"
            );
        }
        if row.template.is_some() {
            for key in &keys {
                assert!(
                    key.starts_with(TEMPLATE_PREFIX),
                    "case {case:?}: expected every key under {TEMPLATE_PREFIX:?}, got {key:?}"
                );
            }
        }
    }

    // A channel keeps reading its stream, so a batch written after the first delivery is
    // delivered too — and the first batch is not delivered a second time.
    let row = rows.first().expect("the table holds a row");
    let later: Vec<String> = (0..4)
        .map(|index| payload("onerecord-later", index, 0))
        .collect();
    let stream_arn = format!("arn:aws:kinesis:{REGION}:{account}:stream/{}", row.name);
    kinesis_ok(
        &router,
        row.case,
        "PutRecords",
        &batch(&stream_arn, &later),
    )
    .await;
    let expected = row.payloads.len() + later.len();
    let (keys, records) = poll_until_delivered(&router, row.case, row.name, expected).await;
    assert_eq!(
        records.len(),
        expected,
        "case {:?}: a later batch is delivered beside the first and nothing twice, got \
         {records:?} under the keys {keys:?}",
        row.case
    );
    for payload in row.payloads.iter().chain(later.iter()) {
        let found = records.iter().filter(|record| *record == payload).count();
        assert_eq!(
            found, 1,
            "case {:?}: expected {payload:?} delivered exactly once, got {found} times \
             under the keys {keys:?}",
            row.case
        );
    }
    assert!(
        keys.len() >= 2,
        "case {:?}: two batches delivered at two instants are two objects, got {keys:?}",
        row.case
    );
}
