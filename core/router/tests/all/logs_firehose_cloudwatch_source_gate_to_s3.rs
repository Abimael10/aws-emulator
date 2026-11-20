// SPDX-License-Identifier: CC0-1.0

//! A Firehose delivery stream that enables the CloudWatch Logs processing pipeline takes the
//! records CloudWatch Logs forwards for a subscription filter and refuses a direct API put,
//! end to end: CloudWatch Logs -> Firehose -> Amazon S3.
//!
//! The rule under test: which producer a Firehose delivery stream accepts data from is decided
//! by the stream's destination processing configuration and by WHO is calling, never by the
//! bytes — a stream whose enabled `ProcessingConfiguration` carries a processor of the
//! CloudWatch Logs pipeline (`Decompression`, `CloudWatchLogProcessing`) takes records from
//! CloudWatch Logs and refuses every other producer with `InvalidSourceException` (HTTP 400),
//! the exception whose own documentation states the scope — "Only requests from CloudWatch Logs
//! are supported when CloudWatch Logs decompression is enabled" — while the SENTENCE the
//! refusal carries is the data plane's own ("Put to Firehose failed for AccountId: …,
//! FirehoseName: … because the request is not originating from CloudWatch.", stated at
//! [`expected_refusal`] from the account the put was scoped to and the stream it named);
//! any other stream takes a direct put as well — and in BOTH cases what the stream
//! accepted, and only that, reaches the stream's Amazon S3 destination.
//!
//! Only a cross-service suite can pin this: the claim spans the forwarding hop a subscription
//! filter owns (CloudWatch Logs), the gate and the buffer the stream owns (Firehose), the role
//! both writes are made under (IAM) and the objects the delivery writes (Amazon S3) — the four
//! services on ONE engine, exactly as `core/router/src/lib.rs` mounts them. The sibling suite
//! `logs_subscription_filter_to_firehose_s3` pins what a delivered object HOLDS once the
//! destination's processing has run; this one pins who may put records on the stream at all,
//! and that a refused put leaves nothing in the bucket.
//!
//! The rows differ along what the rule generalises over: the destination's processing
//! configuration (absent, present but disabled, decompression alone, the whole pipeline), and
//! so along whether the gate is in force; each row is driven through BOTH direct-put
//! operations (`PutRecord`, `PutRecordBatch`), both of which declare the refusal.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode};

/// The Region every call below is signed for, and the one every ARN here names.
const REGION: &str = "us-west-2";

/// The bucket every stream here delivers into.
const BUCKET: &str = "cwl-firehose-source-gate";

/// The role the forwarding and the delivery are both made under.
const ROLE: &str = "cwl-firehose-source-gate-role";

/// The log stream every row writes its events to.
const LOG_STREAM: &str = "instance-one";

/// The subscription filter every row configures.
const FILTER: &str = "to-firehose";

/// How many log events each row's `PutLogEvents` carries.
const EVENTS: usize = 12;

/// The trust policy the delivery role carries: CloudWatch Logs forwards under it and Firehose
/// delivers under it, so both service principals may assume it.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"#,
    r#"["logs.amazonaws.com","firehose.amazonaws.com"]},"Action":"sts:AssumeRole"}]}"#
);

/// The role's permissions: the record write CloudWatch Logs makes on the delivery stream
/// (`PutSubscriptionFilter$roleArn`, "grants CloudWatch Logs permissions to deliver ingested
/// log events to the destination stream") and the object write the delivery makes.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["firehose:PutRecord","firehose:PutRecordBatch","s3:PutObject"],"Resource":"*"}]}"#
);

/// How long a stream is waited for to leave `CREATING`, and a delivery to appear: far longer
/// than the emulator's own spans, so a loaded machine does not fail a row the clock would have
/// served.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often a condition is looked at again while waiting for it.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What a subscription-filter delivery leaves in the destination object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Payload {
    /// The gzip frames CloudWatch Logs delivered, as they arrived.
    GzipFrames,
    /// Those frames inflated: the envelope documents themselves.
    Envelopes,
    /// Only the `message` of each forwarded log event, one per line.
    Messages,
}

/// One row: a delivery stream, the processing its destination configures, and whether that
/// configuration puts the CloudWatch Logs source gate in force.
struct Row {
    case: &'static str,
    /// Names this row's stream, log group and destination prefix, so rows cannot collide.
    name: &'static str,
    /// The destination's `ProcessingConfiguration`, or `None` for a destination that
    /// configures none.
    processing: Option<Value>,
    /// Whether a direct `PutRecord` / `PutRecordBatch` must be refused with
    /// `InvalidSourceException`.
    gated: bool,
    /// What the subscription-filter delivery leaves in the object.
    expect: Payload,
}

/// The `Decompression` processor, with the compression format CloudWatch Logs frames its
/// payload in. Source: `smithy_query(query="shape", shape="ProcessorType,
/// ProcessorParameterName")`.
fn decompression() -> Value {
    json!({
        "Type": "Decompression",
        "Parameters": [{ "ParameterName": "CompressionFormat", "ParameterValue": "GZIP" }],
    })
}

/// The `CloudWatchLogProcessing` processor asking for message extraction.
fn message_extraction() -> Value {
    json!({
        "Type": "CloudWatchLogProcessing",
        "Parameters": [{ "ParameterName": "DataMessageExtraction", "ParameterValue": "true" }],
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a destination configuring no data processing at all",
            name: "open-untouched",
            processing: None,
            gated: false,
            expect: Payload::GzipFrames,
        },
        Row {
            case: "the whole pipeline configured but disabled",
            name: "open-disabled",
            processing: Some(json!({
                "Enabled": false,
                "Processors": [decompression(), message_extraction()],
            })),
            gated: false,
            expect: Payload::GzipFrames,
        },
        Row {
            case: "decompression alone",
            name: "gated-inflating",
            processing: Some(json!({ "Enabled": true, "Processors": [decompression()] })),
            gated: true,
            expect: Payload::Envelopes,
        },
        Row {
            case: "decompression with message extraction",
            name: "gated-extracting",
            processing: Some(json!({
                "Enabled": true,
                "Processors": [decompression(), message_extraction()],
            })),
            gated: true,
            expect: Payload::Messages,
        },
    ]
}

/// The log group the row `name` subscribes.
fn group_of(name: &str) -> String {
    format!("/aws/emulator/{name}")
}

/// The message of event `index` of the row `name` writes: distinct per row and per event, so
/// an assertion names exactly what it did or did not find.
fn message(name: &str, index: usize) -> String {
    format!("ERROR {name} event {index}")
}

/// The payload a DIRECT put at the row `name` carries, through the operation `operation`:
/// distinct per row and per operation, so the bucket can be searched for exactly the bytes one
/// put offered.
fn direct_payload(name: &str, operation: &str) -> String {
    format!("DIRECT-PUT-{operation}-AT-{name}")
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

/// One `awsJson1_1` operation: its status and its parsed answer, whatever the status is.
async fn json_try(
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
    let parsed = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, parsed)
}

/// One `awsJson1_1` operation that must succeed.
async fn json_call(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target: &str,
    body: Value,
) -> Value {
    let (status, answered) = json_try(router, signing_name, target, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {target} must succeed, got {status} {answered:?}"
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

/// The wire error code of a refusal: the `__type` an `awsJson1_1` error body carries, without
/// the namespace that may qualify it.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message of a refusal.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The refusal a direct put to a CloudWatch-Logs-only stream reads.
///
/// Stated here from the two facts the sentence is about — the account the put was scoped to
/// and the stream it named — in the documented sentence form, so the assertion is a statement
/// about the message rather than a read of the code under test.
fn expected_refusal(stream: &str) -> String {
    format!(
        "Put to Firehose failed for AccountId: {}, FirehoseName: {stream} \
         because the request is not originating from CloudWatch.",
        engine::emulator_account_id()
    )
}

/// Every key the bucket holds under `prefix`, read through `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
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

/// The bytes the bucket holds at `key`, read through `GetObject`.
async fn object(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<u8> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{BUCKET}/{key}"),
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

/// Every object under `prefix`, concatenated in key order.
async fn held_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for key in keys_under(router, case, prefix).await {
        body.extend_from_slice(&object(router, case, &key).await);
    }
    body
}

/// Everything under `prefix` once it holds `wanted`, or a failure naming the row once
/// [`DEADLINE`] has passed. The CONDITION is polled for; nothing here waits a fixed span.
async fn delivered_holding(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    wanted: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = held_under(router, case, prefix).await;
        if String::from_utf8_lossy(&body).contains(wanted) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivered object under {prefix:?} holding {wanted:?}, \
             got {} byte(s)",
            body.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Everything under `prefix` once at least one object is there, or a failure naming the row
/// once [`DEADLINE`] has passed.
async fn delivered(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<u8> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if !keys_under(router, case, prefix).await.is_empty() {
            return held_under(router, case, prefix).await;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least one delivered object under {prefix:?}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait for `stream` to report `ACTIVE`, polling `DescribeDeliveryStream` for the status
/// rather than waiting out the emulator's creation span.
async fn wait_active(router: &MultiServiceRouter, case: &str, stream: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = json_call(
            router,
            case,
            "firehose",
            "Firehose_20150804.DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == "ACTIVE" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stream:?} to reach ACTIVE, it is still {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One record body, base64 as the wire blob requires.
fn record(payload: &str) -> Value {
    json!({ "Data": aws_smithy_types::base64::encode(payload.as_bytes()) })
}

/// The emulator the suite drives: the four services the flow spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
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

#[tokio::test]
async fn a_cloudwatch_logs_pipeline_stream_delivers_the_filters_events_and_refuses_a_direct_put() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let role_arn = format!("arn:aws:iam::{account}:role/{ROLE}");
    let rows = rows();

    // The role both writes are made under, and the bucket every stream delivers into.
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

    // Every row's stream, log group and log stream, created together so the creation spans
    // they arm run side by side.
    for row in &rows {
        let mut destination = json!({
            "RoleARN": role_arn,
            "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
            "Prefix": format!("{}/", row.name),
            // The shortest buffering the hints admit, so a row waits for the emulator's own
            // delivery span and nothing longer.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
        });
        if let Some(processing) = &row.processing {
            destination["ProcessingConfiguration"] = processing.clone();
        }
        json_call(
            &router,
            row.case,
            "firehose",
            "Firehose_20150804.CreateDeliveryStream",
            json!({
                "DeliveryStreamName": row.name,
                "DeliveryStreamType": "DirectPut",
                "ExtendedS3DestinationConfiguration": destination,
            }),
        )
        .await;
        json_call(
            &router,
            row.case,
            "logs",
            "Logs_20140328.CreateLogGroup",
            json!({ "logGroupName": group_of(row.name) }),
        )
        .await;
        json_call(
            &router,
            row.case,
            "logs",
            "Logs_20140328.CreateLogStream",
            json!({
                "logGroupName": group_of(row.name),
                "logStreamName": LOG_STREAM,
            }),
        )
        .await;
    }
    for row in &rows {
        wait_active(&router, row.case, row.name).await;
    }

    // Each log group subscribes its stream and ingests one batch of events.
    for row in &rows {
        json_call(
            &router,
            row.case,
            "logs",
            "Logs_20140328.PutSubscriptionFilter",
            json!({
                "logGroupName": group_of(row.name),
                "filterName": FILTER,
                "filterPattern": "",
                "destinationArn":
                    format!("arn:aws:firehose:{REGION}:{account}:deliverystream/{}", row.name),
                "roleArn": role_arn,
            }),
        )
        .await;
        // One ingestion instant for the whole batch, inside the ingestion window.
        let timestamp = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after the Unix epoch")
                .as_millis(),
        )
        .expect("the epoch milliseconds fit in an i64")
            - 60 * 60 * 1_000;
        let events: Vec<Value> = (0..EVENTS)
            .map(|index| json!({ "timestamp": timestamp, "message": message(row.name, index) }))
            .collect();
        json_call(
            &router,
            row.case,
            "logs",
            "Logs_20140328.PutLogEvents",
            json!({
                "logGroupName": group_of(row.name),
                "logStreamName": LOG_STREAM,
                "logEvents": events,
            }),
        )
        .await;
    }

    // Every destination receives what CloudWatch Logs forwarded — the gated streams included,
    // which is the producer they do take data from.
    for row in &rows {
        let case = row.case;
        let body = delivered(&router, case, &format!("{}/", row.name)).await;
        let text = String::from_utf8_lossy(&body).to_string();
        match row.expect {
            Payload::GzipFrames => {
                assert_eq!(
                    body.get(..2),
                    Some([0x1f, 0x8b].as_slice()),
                    "case {case:?}: an untransformed record keeps the gzip framing CloudWatch \
                     Logs delivers, got {:?}",
                    body.get(..2)
                );
            }
            Payload::Envelopes => {
                assert!(
                    text.contains("\"messageType\":\"DATA_MESSAGE\"")
                        && text.contains(&format!("\"logGroup\":\"{}\"", group_of(row.name))),
                    "case {case:?}: an inflated record is the envelope document, got {text:?}"
                );
                for index in 0..EVENTS {
                    let message = message(row.name, index);
                    assert!(
                        text.contains(&message),
                        "case {case:?}: the inflated envelope carries {message:?}, got {text:?}"
                    );
                }
            }
            Payload::Messages => {
                assert!(
                    !text.contains("\"messageType\""),
                    "case {case:?}: extraction leaves only the messages, got {text:?}"
                );
                for index in 0..EVENTS {
                    let message = message(row.name, index);
                    assert!(
                        text.contains(&format!("{message}\n")),
                        "case {case:?}: the object carries the extracted {message:?}, got {text:?}"
                    );
                }
            }
        }
    }

    // Now the gate itself, through both direct-put operations, on streams that have just been
    // proven to be taking records from CloudWatch Logs.
    for row in &rows {
        let case = row.case;
        let single = direct_payload(row.name, "PutRecord");
        let batched = direct_payload(row.name, "PutRecordBatch");
        let (put_status, put) = json_try(
            &router,
            "firehose",
            "Firehose_20150804.PutRecord",
            json!({ "DeliveryStreamName": row.name, "Record": record(&single) }),
        )
        .await;
        let (batch_status, batch) = json_try(
            &router,
            "firehose",
            "Firehose_20150804.PutRecordBatch",
            json!({ "DeliveryStreamName": row.name, "Records": [record(&batched)] }),
        )
        .await;

        if row.gated {
            let expected = expected_refusal(row.name);
            for (operation, status, answer) in [
                ("PutRecord", put_status, &put),
                ("PutRecordBatch", batch_status, &batch),
            ] {
                assert_eq!(
                    status, 400,
                    "case {case:?}: {operation} must be refused, got {status} {answer:?}"
                );
                let code = error_code(answer);
                assert_eq!(
                    code, "InvalidSourceException",
                    "case {case:?}: {operation} expected InvalidSourceException, got {code:?} \
                     in {answer:?}"
                );
                let message = error_message(answer);
                assert_eq!(
                    message, expected,
                    "case {case:?}: {operation} expected {expected:?}, got {message:?}"
                );
            }
        } else {
            assert_eq!(
                put_status, 200,
                "case {case:?}: PutRecord must be accepted, got {put_status} {put:?}"
            );
            assert!(
                put["RecordId"].as_str().is_some_and(|id| !id.is_empty()),
                "case {case:?}: an accepted PutRecord answers a RecordId, got {put:?}"
            );
            assert_eq!(
                batch_status, 200,
                "case {case:?}: PutRecordBatch must be accepted, got {batch_status} {batch:?}"
            );
            assert_eq!(
                batch["FailedPutCount"],
                json!(0),
                "case {case:?}: no record of the batch fails, got {batch:?}"
            );
        }
    }

    // What an UNGATED stream took is delivered to its destination, which is also the signal
    // that a flush has come due since the puts above were made.
    for row in rows.iter().filter(|row| !row.gated) {
        for operation in ["PutRecord", "PutRecordBatch"] {
            let wanted = direct_payload(row.name, operation);
            delivered_holding(&router, row.case, &format!("{}/", row.name), &wanted).await;
        }
    }

    // And what a GATED stream refused is nowhere in the bucket: not under its own prefix, not
    // under an error output, not anywhere.
    let held = held_under(&router, "the gate", "").await;
    let whole_bucket = String::from_utf8_lossy(&held).to_string();
    for row in rows.iter().filter(|row| row.gated) {
        for operation in ["PutRecord", "PutRecordBatch"] {
            let refused = direct_payload(row.name, operation);
            assert!(
                !whole_bucket.contains(&refused),
                "case {:?}: the refused {operation} payload {refused:?} must reach no object of \
                 the bucket, found it in {} byte(s)",
                row.case,
                whole_bucket.len()
            );
        }
    }
}
