// SPDX-License-Identifier: CC0-1.0

//! A Kinesis-sourced Firehose stream retrieves its source only where the role that source
//! names may make the read — and reports a read it may not make where the stream's logging
//! says.
//!
//! The rule under test: **Firehose retrieves a `KinesisStreamAsSource` stream's data from the
//! source under the role its `KinesisStreamSourceConfiguration` names ("the ARN of the role
//! that provides access to the source stream" — `RoleARN`), so the retrieval happens only
//! where the permissions in force on THAT role allow the Kinesis Data Streams calls the read
//! is made of (`kinesis:DescribeStream`, `kinesis:GetShardIterator`, `kinesis:GetRecords`). A
//! read the role may not make retrieves nothing, so nothing reaches the destination, and —
//! where the destination's `CloudWatchLoggingOptions` are enabled — it is reported as a log
//! event in the `LogGroupName` / `LogStreamName` they name, whose `errorCode` is the fixed
//! `Kinesis.AccessDenied` and whose message names the refused call, the source stream and the
//! role. A read the role may make retrieves the records and reports nothing.**
//!
//! That log event is the ONLY thing observable about such a stream: no client call is made
//! for a source-fed stream at all, so a stream whose role cannot read its source is otherwise
//! indistinguishable from an idle, healthy one.
//!
//! Only a cross-service suite can ask this: the flow spans Amazon Kinesis Data Streams (the
//! source), IAM (which holds the role and decides the read), Firehose (which retrieves,
//! buffers, authorizes and delivers), Amazon S3 (which holds the destination objects) and
//! CloudWatch Logs (which holds the reported events), mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them. The sibling suite
//! `cloudformation_kinesis_sourced_firehose_delivery_role_authorization` pins the same
//! question for the DESTINATION write of such a stream.
//!
//! The rows differ along what the rule generalises over: which of the three source read calls
//! the role allows (all of them, all of them by wildcard, every one but the records read,
//! every one but the describe, none at all), whether the grant is scoped to the source stream
//! or to another stream, and whether the destination's logging options report anything. The
//! second test adds the two things one stack shows: a permitted SIBLING of the refused stream,
//! reading the same source, delivers while the refused one writes nothing — and granting the
//! missing permission in place restores the refused stream's delivery.
//!
//! Nothing here is asserted about how fast the host ran: every wait polls for the CONDITION
//! under a generous deadline, and what must stay empty is re-read at every one of those
//! readings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode};

/// The Region every call below is signed for, and the one the bucket is created in.
const REGION: &str = "us-west-2";

/// How long a delivery, or the report of a read that was refused, is waited for before a case
/// fails: far longer than the emulator's own `CREATING` -> `ACTIVE` span, source poll and
/// buffer window together, so a loaded machine never fails a case the clock would have
/// served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(90);

/// How often a status, a destination or a log stream is read again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long what must hold NOTHING goes on being re-read once the case's own condition holds
/// — spent re-reading rather than sleeping once, so the assertions judge the latest reading
/// and cover several source polls and buffer windows.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// The trust policy every role here carries: Firehose assumes the role a stream's source and
/// destination name, so the service principal has to be able to.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// Amazon S3's own write of an object, which is the action the destination write of these
/// streams is authorized under — allowed by every role here, so the ONE difference between
/// the rows is what each may read from the source.
const OBJECT_WRITE: &str = "s3:PutObject";

/// The three Kinesis Data Streams calls the retrieval from a source stream is made of.
const EVERY_SOURCE_READ: &[&str] = &[
    "kinesis:DescribeStream",
    "kinesis:GetShardIterator",
    "kinesis:GetRecords",
];

/// The `errorCode` a source read the stream's role may not make is reported under — fixed
/// text.
const REFUSED_ERROR_CODE: &str = "Kinesis.AccessDenied";

/// The log group the destinations of this suite report into.
const LOG_GROUP: &str = "/aws/kinesisfirehose/source-read";

/// The log stream a stream's own destination logging names.
const LOG_STREAM: &str = "DestinationDelivery";

/// Where the delivered objects of a stream go.
const PREFIX: &str = "data/";

/// The five services this flow spans, on ONE shared engine, with the request builders the
/// suite drives them through.
struct Aws {
    router: MultiServiceRouter,
}

impl Aws {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One request to the service `signing_name`; answers its status and body bytes.
    async fn send(
        &self,
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
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().to_vec())
    }

    /// One `awsJson1_1` operation of `signing_name`, answering its status and parsed body.
    async fn json_rpc(&self, signing_name: &str, target: &str, body: Value) -> (u16, Value) {
        let (status, answered) = self
            .send(
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
    async fn kinesis(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, answered) = self
            .json_rpc("kinesis", &format!("Kinesis_20131202.{operation}"), body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: Kinesis {operation} must succeed, got {status} {answered}"
        );
        answered
    }

    /// One Firehose operation that must succeed.
    async fn firehose(&self, case: &str, operation: &str, body: Value) -> Value {
        let (status, answered) = self
            .json_rpc("firehose", &format!("Firehose_20150804.{operation}"), body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: Firehose {operation} must succeed, got {status} {answered}"
        );
        answered
    }

    /// One CloudWatch Logs operation, with whatever it answered.
    async fn logs(&self, operation: &str, body: Value) -> (u16, Value) {
        self.json_rpc("logs", &format!("Logs_20140328.{operation}"), body)
            .await
    }

    /// One `awsQuery` IAM action that must succeed.
    async fn iam(&self, case: &str, action: &str, parameters: &[(&str, &str)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (key, value) in parameters {
            body.push('&');
            body.push_str(key);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let (status, answered) = self
            .send(
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

    /// Create the role `name`, trusted by the Firehose service principal and carrying
    /// `statements` as its one inline policy — or no policy at all when `statements` is
    /// empty. Answers its ARN.
    async fn given_role(&self, case: &str, name: &str, statements: &[String]) -> String {
        self.iam(
            case,
            "CreateRole",
            &[("RoleName", name), ("AssumeRolePolicyDocument", TRUST_POLICY)],
        )
        .await;
        if !statements.is_empty() {
            let document = format!(
                r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
                statements.join(",")
            );
            self.iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", name),
                    ("PolicyName", "firehose-source"),
                    ("PolicyDocument", &document),
                ],
            )
            .await;
        }
        format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
    }

    /// Create the destination bucket `name`.
    async fn given_bucket(&self, case: &str, name: &str) {
        let (status, body) = self
            .send(
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
            "case {case:?}: the destination bucket {name} must be created, got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Open the log group and the log streams the destinations report into, as an operator
    /// does before pointing a stream's logging at them.
    async fn given_log_group(&self, case: &str, streams: &[&str]) {
        let (status, answered) = self
            .logs("CreateLogGroup", json!({ "logGroupName": LOG_GROUP }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the log group must be created, got {status} {answered}"
        );
        for stream in streams {
            let (status, answered) = self
                .logs(
                    "CreateLogStream",
                    json!({ "logGroupName": LOG_GROUP, "logStreamName": stream }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the log stream {stream} must be created, got {status} {answered}"
            );
        }
    }

    /// Create the source Kinesis data stream `name` and answer the `StreamARN` it reports
    /// once it is `ACTIVE`.
    async fn given_source(&self, case: &str, name: &str) -> String {
        self.kinesis(
            case,
            "CreateStream",
            json!({ "StreamName": name, "ShardCount": 1 }),
        )
        .await;
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let described = self
                .kinesis(case, "DescribeStreamSummary", json!({ "StreamName": name }))
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

    /// Create the `KinesisStreamAsSource` Firehose stream `name` reading `source_arn` under
    /// `role_arn` and delivering into `bucket` under `prefix`, and answer its ARN once it
    /// reports `ACTIVE`.
    #[allow(clippy::too_many_arguments)]
    async fn given_stream(
        &self,
        case: &str,
        name: &str,
        source_arn: &str,
        role_arn: &str,
        bucket: &str,
        prefix: &str,
        logging: Option<&str>,
    ) -> String {
        let mut destination = json!({
            "RoleARN": role_arn,
            "BucketARN": format!("arn:aws:s3:::{bucket}"),
            "Prefix": prefix,
            // The smallest size and the shortest interval the member admits, so a case waits
            // for the retrieval and not for a buffer window it did not ask to test.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
        });
        destination["CloudWatchLoggingOptions"] = match logging {
            Some(log_stream) => json!({
                "Enabled": true,
                "LogGroupName": LOG_GROUP,
                "LogStreamName": log_stream,
            }),
            None => json!({
                "Enabled": false,
                "LogGroupName": LOG_GROUP,
                "LogStreamName": LOG_STREAM,
            }),
        };
        let answered = self
            .firehose(
                case,
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": name,
                    "DeliveryStreamType": "KinesisStreamAsSource",
                    "KinesisStreamSourceConfiguration": {
                        "KinesisStreamARN": source_arn,
                        "RoleARN": role_arn,
                    },
                    "ExtendedS3DestinationConfiguration": destination,
                }),
            )
            .await;
        let arn = answered["DeliveryStreamARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create answers {name}'s ARN, got {answered}"))
            .to_string();
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let described = self
                .firehose(
                    case,
                    "DescribeDeliveryStream",
                    json!({ "DeliveryStreamName": name }),
                )
                .await;
            if described["DeliveryStreamDescription"]["DeliveryStreamStatus"] == json!("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {name} must reach ACTIVE, got {described}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Write `count` records to the source, as a producer does.
    async fn put_records(&self, case: &str, source: &str, count: usize) {
        let entries: Vec<Value> = (0..count)
            .map(|index| {
                json!({
                    "PartitionKey": format!("pk-{index}"),
                    "Data": ::aws_smithy_types::base64::encode(
                        format!("record-{index}\n").as_bytes(),
                    ),
                })
            })
            .collect();
        let answered = self
            .kinesis(
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
    }

    /// Every key `bucket` holds under `prefix`, read through `ListObjectsV2`.
    async fn keys_under(&self, case: &str, bucket: &str, prefix: &str) -> Vec<String> {
        let (status, body) = self
            .send(
                "s3",
                "GET",
                // Every prefix here is made of unreserved characters and `/`, so it travels in
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

    /// The events `LOG_GROUP`/`stream` holds, read through `GetLogEvents`.
    async fn log_events(&self, stream: &str) -> Vec<Value> {
        let (status, answered) = self
            .logs(
                "GetLogEvents",
                json!({
                    "logGroupName": LOG_GROUP,
                    "logStreamName": stream,
                    "startFromHead": true,
                }),
            )
            .await;
        if status != 200 {
            return Vec::new();
        }
        answered["events"].as_array().cloned().unwrap_or_default()
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. Neither the key
/// id nor the signature is a credential: this router runs with request authentication off, so
/// nothing here is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// An identity-policy statement allowing `actions` on `resource`.
fn allow(actions: &[&str], resource: &str) -> String {
    let actions = actions
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"Effect":"Allow","Action":[{actions}],"Resource":"{resource}"}}"#)
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// Which of the source read calls the role's policy allows; empty for a role that carries
    /// no source grant at all.
    source_actions: &'static [&'static str],
    /// Whether that grant names the source stream (`true`) or ANOTHER stream (`false`).
    scoped_to_the_source: bool,
    /// Whether the destination's `CloudWatchLoggingOptions` are enabled.
    logging: bool,
    /// The refused call the report names, or `None` where the read is made.
    refused: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role allowing the three source read calls retrieves and reports nothing",
            source_actions: EVERY_SOURCE_READ,
            scoped_to_the_source: true,
            logging: true,
            refused: None,
        },
        Row {
            case: "a role allowing every Kinesis call by wildcard retrieves",
            source_actions: &["kinesis:*"],
            scoped_to_the_source: true,
            logging: true,
            refused: None,
        },
        Row {
            case: "a role that may not read the records reports the refused records read",
            source_actions: &["kinesis:DescribeStream", "kinesis:GetShardIterator"],
            scoped_to_the_source: true,
            logging: true,
            refused: Some("kinesis:GetRecords"),
        },
        Row {
            case: "a role that may not describe the source reports the refused describe",
            source_actions: &["kinesis:GetShardIterator", "kinesis:GetRecords"],
            scoped_to_the_source: true,
            logging: true,
            refused: Some("kinesis:DescribeStream"),
        },
        Row {
            case: "a role whose source grant names another stream is refused",
            source_actions: EVERY_SOURCE_READ,
            scoped_to_the_source: false,
            logging: true,
            refused: Some("kinesis:DescribeStream"),
        },
        Row {
            case: "a role carrying no source grant at all is refused",
            source_actions: &[],
            scoped_to_the_source: true,
            logging: true,
            refused: Some("kinesis:DescribeStream"),
        },
        Row {
            case: "a refused read reports nothing where the logging options are disabled",
            source_actions: &["kinesis:DescribeStream", "kinesis:GetShardIterator"],
            scoped_to_the_source: true,
            logging: false,
            refused: Some("kinesis:GetRecords"),
        },
    ]
}

/// The events of `stream` once at least one is there, or a failure naming the row once
/// [`DELIVERY_DEADLINE`] has passed.
async fn reported(aws: &Aws, case: &str, stream: &str) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let events = aws.log_events(stream).await;
        if !events.is_empty() {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a reported source read error in {stream} within \
             {DELIVERY_DEADLINE:?}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The keys `bucket` holds under `prefix` once at least one is there, or a failure naming the
/// row.
async fn delivered(aws: &Aws, case: &str, bucket: &str, prefix: &str) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let keys = aws.keys_under(case, bucket, prefix).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivered object under {prefix} within \
             {DELIVERY_DEADLINE:?}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The document one reported event carries.
fn document_of(case: &str, events: &[Value]) -> Value {
    let message = events[0]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a reported event carries a message"))
        .to_string();
    serde_json::from_str(&message).unwrap_or_else(|error| {
        panic!("case {case:?}: the event is a JSON document: {error} in {message}")
    })
}

#[tokio::test]
async fn a_source_read_the_role_may_not_make_is_reported_and_retrieves_nothing() {
    for row in rows() {
        let case = row.case;
        let aws = Aws::new();
        let bucket = "firehose-source-read";
        aws.given_bucket(case, bucket).await;
        aws.given_log_group(case, &[LOG_STREAM]).await;
        let source_arn = aws.given_source(case, "ingest").await;
        let mut statements = vec![allow(&[OBJECT_WRITE], &format!("arn:aws:s3:::{bucket}/*"))];
        if !row.source_actions.is_empty() {
            let resource = if row.scoped_to_the_source {
                source_arn.clone()
            } else {
                format!("arn:aws:kinesis:{REGION}:{}:stream/elsewhere", engine::emulator_account_id())
            };
            statements.push(allow(row.source_actions, &resource));
        }
        let role_arn = aws.given_role(case, "firehose-source", &statements).await;
        let stream_arn = aws
            .given_stream(
                case,
                "archiver",
                &source_arn,
                &role_arn,
                bucket,
                PREFIX,
                row.logging.then_some(LOG_STREAM),
            )
            .await;
        aws.put_records(case, "ingest", 3).await;

        match (row.refused, row.logging) {
            (None, _) => {
                let keys = delivered(&aws, case, bucket, PREFIX).await;
                assert!(
                    keys.iter().all(|key| key.starts_with(PREFIX)),
                    "case {case:?}: every delivered object goes under {PREFIX:?}, got {keys:?}"
                );
                // A retrieval that was made reports nothing at all, which is what tells a
                // healthy stream from a misconfigured one.
                let deadline = Instant::now() + SILENCE_WINDOW;
                while Instant::now() < deadline {
                    let events = aws.log_events(LOG_STREAM).await;
                    assert!(
                        events.is_empty(),
                        "case {case:?}: a retrieval that was made reports nothing, got {events:?}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
            (Some(action), true) => {
                let events = reported(&aws, case, LOG_STREAM).await;
                let document = document_of(case, &events);
                assert_eq!(
                    document["errorCode"],
                    json!(REFUSED_ERROR_CODE),
                    "case {case:?}: expected errorCode {REFUSED_ERROR_CODE:?}, got {document}"
                );
                assert_eq!(
                    document["deliveryStreamARN"],
                    json!(stream_arn),
                    "case {case:?}: the event names the stream the read was for, got {document}"
                );
                assert_eq!(
                    document["deliveryStreamVersionId"],
                    json!(1),
                    "case {case:?}: the event names the stream's version, got {document}"
                );
                let message = document["message"].as_str().unwrap_or_default().to_string();
                for named in [action, source_arn.as_str(), role_arn.as_str()] {
                    assert!(
                        message.contains(named),
                        "case {case:?}: expected the message to name {named:?}, got {message:?}"
                    );
                }
                // Nothing of a refused retrieval reaches the destination, at every reading.
                let deadline = Instant::now() + SILENCE_WINDOW;
                while Instant::now() < deadline {
                    let keys = aws.keys_under(case, bucket, PREFIX).await;
                    assert!(
                        keys.is_empty(),
                        "case {case:?}: a refused retrieval delivers nothing, got {keys:?}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
            (Some(_), false) => {
                // Read over the window the delivery and the report would have used, so this is
                // the absence of both rather than either still being owed.
                let deadline = Instant::now() + SILENCE_WINDOW;
                while Instant::now() < deadline {
                    let events = aws.log_events(LOG_STREAM).await;
                    assert!(
                        events.is_empty(),
                        "case {case:?}: options that are switched off report nothing, got \
                         {events:?}"
                    );
                    let keys = aws.keys_under(case, bucket, PREFIX).await;
                    assert!(
                        keys.is_empty(),
                        "case {case:?}: a refused retrieval delivers nothing, got {keys:?}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }
    }
}

#[tokio::test]
async fn the_permitted_sibling_delivers_and_granting_the_permission_restores_the_refused_stream() {
    let case = "two streams on one source, one role short of the records read";
    let aws = Aws::new();
    let bucket = "firehose-source-read-siblings";
    let good_stream = "GoodLogStream";
    let bad_stream = "BadLogStream";
    aws.given_bucket(case, bucket).await;
    aws.given_log_group(case, &[good_stream, bad_stream]).await;
    let source_arn = aws.given_source(case, "shared-ingest").await;
    let object_write = allow(&[OBJECT_WRITE], &format!("arn:aws:s3:::{bucket}/*"));

    // One role may make the whole source read; the other may make everything but the records
    // read, which is the ONE difference between the two streams.
    let permitted = aws
        .given_role(
            case,
            "firehose-permitted",
            &[
                object_write.clone(),
                allow(EVERY_SOURCE_READ, &source_arn),
            ],
        )
        .await;
    let refused = aws
        .given_role(
            case,
            "firehose-refused",
            &[
                object_write.clone(),
                allow(
                    &["kinesis:DescribeStream", "kinesis:GetShardIterator"],
                    &source_arn,
                ),
            ],
        )
        .await;

    aws.given_stream(
        case,
        "good",
        &source_arn,
        &permitted,
        bucket,
        "good/",
        Some(good_stream),
    )
    .await;
    aws.given_stream(
        case,
        "bad",
        &source_arn,
        &refused,
        bucket,
        "bad/",
        Some(bad_stream),
    )
    .await;
    aws.put_records(case, "shared-ingest", 3).await;

    // The permitted sibling delivers the whole record set.
    let keys = delivered(&aws, case, bucket, "good/").await;
    assert!(
        keys.iter().all(|key| key.starts_with("good/")),
        "case {case:?}: the permitted sibling delivers under its own prefix, got {keys:?}"
    );
    // The refused one reports the read it could not make and delivers nothing.
    let events = reported(&aws, case, bad_stream).await;
    let document = document_of(case, &events);
    assert_eq!(
        document["errorCode"],
        json!(REFUSED_ERROR_CODE),
        "case {case:?}: expected errorCode {REFUSED_ERROR_CODE:?}, got {document}"
    );
    let deadline = Instant::now() + SILENCE_WINDOW;
    while Instant::now() < deadline {
        let keys = aws.keys_under(case, bucket, "bad/").await;
        assert!(
            keys.is_empty(),
            "case {case:?}: the refused stream delivers nothing, got {keys:?}"
        );
        let events = aws.log_events(good_stream).await;
        assert!(
            events.is_empty(),
            "case {case:?}: the permitted sibling reports nothing, got {events:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Granting the missing permission IN PLACE restores the refused stream's retrieval: the
    // records it never took are still in the source, so they are delivered once the read is
    // allowed.
    let repaired = format!(
        r#"{{"Version":"2012-10-17","Statement":[{},{}]}}"#,
        object_write,
        allow(EVERY_SOURCE_READ, &source_arn)
    );
    aws.iam(
        case,
        "PutRolePolicy",
        &[
            ("RoleName", "firehose-refused"),
            ("PolicyName", "firehose-source"),
            ("PolicyDocument", &repaired),
        ],
    )
    .await;
    let keys = delivered(&aws, case, bucket, "bad/").await;
    assert!(
        keys.iter().all(|key| key.starts_with("bad/")),
        "case {case:?}: the repaired stream delivers under its own prefix, got {keys:?}"
    );
}
