// SPDX-License-Identifier: CC0-1.0

//! Where a Kinesis event source mapping puts the record of a batch it gave up on when its
//! failure destination is an Amazon S3 BUCKET, across the services that own the halves of it
//! on the ASSEMBLED emulator: Amazon Kinesis holds the stream and its records, IAM answers for
//! the execution role the poller reads the stream as, CloudFormation archives the inline
//! handler into the function's deployment package, Lambda polls the stream and RUNS that
//! handler, and Amazon S3 takes the object the mapping writes.
//!
//! RULE: `MaximumRetryAttempts` is "Discard records after the specified number of retries", so
//! the attempts one batch gets are bounded by the count the mapping states and once they are
//! spent the batch is discarded; the only account of it is the record written to the resource
//! the mapping's `DestinationConfig$OnFailure` names, for which "you can configure an Amazon
//! SNS topic, Amazon SQS queue, Amazon S3 bucket, or Kafka topic as the destination". The write
//! is the destination service's OWN write, so a BUCKET destination takes one OBJECT per
//! discarded batch: an S3 bucket's ARN names the bucket in its resource field and anything
//! after a `/` is a key prefix inside it, so the object appears under exactly that prefix and
//! its payload is the record describing the BATCH — the function, what ended it, how many
//! attempts it had and what the last answered, beside the stream it was read from, the shard,
//! the sequence-number window it covers, when its records arrived and how many it held.
//!
//! Five crates have to agree for that to hold, so the suite lives here rather than in any one
//! of them: the owning crate's own
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/event_source_mapping_bucket_failure_destination.rs`
//! tables the key and the record over the destination forms and both shard sources; what is
//! pinned HERE is that the object really reaches the object store of the whole emulator, read
//! back over Amazon S3's own wire.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the consumer's stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The names this suite's resources take.
const ROLE_NAME: &str = "kinesis-esm-bucket-destination";
/// See [`ROLE_NAME`].
const STACK: &str = "xsvc-kinesis-esm-bucket-destination";
/// See [`ROLE_NAME`].
const FUNCTION: &str = "kinesis-esm-bucket-destination-consumer";
/// See [`ROLE_NAME`].
const STREAM: &str = "kinesis-esm-bucket-destination-source";
/// See [`ROLE_NAME`].
const BUCKET: &str = "kinesis-esm-bucket-destination-records";

/// The key prefix inside the bucket the mapping's destination names, as its ARN spells it.
const PREFIX: &str = "on-failure";

/// Where Lambda binds the two collections used below.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The actions the poller makes on an Amazon Kinesis data stream, which the execution role
/// has to grant it. Source: the actions AWS's own managed execution-role policy for that
/// event source (`AWSLambdaKinesisExecutionRole`) grants.
const KINESIS_READS: [&str; 6] = [
    "kinesis:GetRecords",
    "kinesis:GetShardIterator",
    "kinesis:DescribeStream",
    "kinesis:DescribeStreamSummary",
    "kinesis:ListShards",
    "kinesis:ListStreams",
];

/// How many retries the mapping allows one batch: none, so the first failed attempt is the
/// last and the batch is discarded.
const RETRIES: i64 = 0;

/// What the record of the discarded batch carries on the WIRE, written out here rather than
/// imported from the emulator so a change to either side of it fails this suite.
///
/// Source: AWS's published on-failure record for a stream event source — the schema version,
/// the condition it reports for a batch whose retries ran out, and the member an Amazon
/// Kinesis batch is described under.
const RECORD_SCHEMA_VERSION: &str = "1.0";
/// See [`RECORD_SCHEMA_VERSION`].
const CONDITION_RETRY_ATTEMPTS_EXHAUSTED: &str = "RetryAttemptsExhausted";
/// See [`RECORD_SCHEMA_VERSION`].
const KINESIS_BATCH_INFO: &str = "KinesisBatchInfo";

/// The `FunctionError` an invocation whose handler raised reports.
const UNHANDLED: &str = "Unhandled";

/// How long an observation of work the CLOCK owes — the stack coming up, the stream becoming
/// `ACTIVE`, the function going `Active`, the mapping settling, the batch being attempted and
/// the object reaching the bucket — waits before giving up, and how long between two looks.
/// Generous: the suite fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(120);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler this suite deploys inline: it RAISES for every batch it is handed, so the
/// mapping gives up on that batch once the attempts it allows are spent.
const FAILING_SOURCE: &str = r#"
import base64


def handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    read = [base64.b64decode(record["kinesis"]["data"]).decode() for record in records]
    raise RuntimeError("this handler cannot process " + ",".join(read))
"#;

/// The emulator as it is ASSEMBLED, on one engine: the object the mapping writes goes to the
/// object store this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so
    // the router can name the service it belongs to.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire, with whatever status it answered.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> (u16, Bytes) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), body)
}

/// One AWS Lambda call over its `restJson1` wire, with whatever status it answered.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, answered) = dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).to_string()))
    };
    (status, parsed)
}

/// One Amazon Kinesis call (`awsJson1_1`) that must answer 200.
async fn kinesis(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, answered) = dispatch(
        router,
        "POST",
        &format!("kinesis.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Kinesis_20131202.{operation}")),
            ("authorization", &authorization("kinesis")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).to_string()))
    };
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from Kinesis {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One PATH-STYLE Amazon S3 request, which is how the object store is addressed, with
/// whatever status and body it answered.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, String) {
    let (status, answered) = dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[("authorization", &authorization("s3"))],
        Bytes::new(),
    )
    .await;
    (status, String::from_utf8_lossy(&answered).to_string())
}

/// One awsQuery call of `signing_name` at `host`, as its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = dispatch(
        router,
        "POST",
        host,
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body),
    )
    .await;
    (status, String::from_utf8_lossy(&answered).to_string())
}

/// One IAM call that must answer 200.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from IAM {action}, got {status} with {body}"
    );
    body
}

/// One CloudFormation call that must answer 200.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// The execution role the function runs as: trusted by Lambda, granting the reads the poller
/// makes on the mapped stream and the write the record of a discarded batch takes. Answers
/// its ARN as IAM reports it.
async fn given_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": KINESIS_READS
                .iter()
                .copied()
                .chain(["s3:PutObject"])
                .collect::<Vec<&str>>(),
            // The stream and the bucket are made after the role, so the grant names the
            // ACTIONS the poller makes rather than the resources of this one run.
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "read-the-stream-and-record-what-failed"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    arn
}

/// The bucket the mapping records a discarded batch in, created over Amazon S3's own wire.
async fn given_bucket(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}")).await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from S3 CreateBucket, got {status} with {body}"
    );
}

/// A one-shard Kinesis data stream, waited out to `ACTIVE`; answers its ARN.
async fn given_stream(router: &MultiServiceRouter) -> String {
    kinesis(
        router,
        "CreateStream",
        &json!({ "StreamName": STREAM, "ShardCount": 1 }),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let summary = kinesis(
            router,
            "DescribeStreamSummary",
            &json!({ "StreamName": STREAM }),
        )
        .await;
        let description = &summary["StreamDescriptionSummary"];
        if description["StreamStatus"] == json!("ACTIVE") {
            return description["StreamARN"]
                .as_str()
                .unwrap_or_else(|| panic!("setup: no StreamARN in {summary}"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "setup: the stream never became ACTIVE, last read {summary}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Deploy the consumer: a stack holding the one function, whose inline code CloudFormation
/// archives into the deployment package Lambda runs. Waited out to `Active`.
async fn given_function(router: &MultiServiceRouter, role_arn: &str) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": 60,
                    "Code": { "ZipFile": FAILING_SOURCE },
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the consumer's stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if status == 200 && configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {FUNCTION} Active, last read {status} {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The mapping between the function and the stream, allowing [`RETRIES`] retries per batch and
/// recording a batch it gives up on as an object of [`BUCKET`] under [`PREFIX`]; answers its
/// UUID and the `FunctionArn` it reports.
async fn given_mapping(router: &MultiServiceRouter, stream_arn: &str) -> (String, String) {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        Some(&json!({
            "FunctionName": FUNCTION,
            "EventSourceArn": stream_arn,
            "StartingPosition": "TRIM_HORIZON",
            "BatchSize": 10,
            "Enabled": true,
            "MaximumRetryAttempts": RETRIES,
            "DestinationConfig": {
                "OnFailure": { "Destination": format!("arn:aws:s3:::{BUCKET}/{PREFIX}") },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: expected HTTP 202 from CreateEventSourceMapping, got {status} with {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no UUID in {created}"))
        .to_string();
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no FunctionArn in {created}"))
        .to_string();
    (uuid, function_arn)
}

/// Every key the bucket holds under [`PREFIX`] right now, as `ListObjectsV2` reports them.
async fn listed_keys(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={PREFIX}/"),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from S3 ListObjectsV2, got {status} with {body}"
    );
    elements(&body, "Key")
}

/// Wait until an object has appeared under the mapping's prefix, answering every key that is
/// there — more than one when more than one arrived, which is what makes "exactly one"
/// assertable.
async fn awaited_keys(router: &MultiServiceRouter) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let keys = listed_keys(router).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "expected the record of the discarded batch under {PREFIX:?} of {BUCKET:?} within \
             {BUDGET:?}, got nothing"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: once the attempts a mapping's `MaximumRetryAttempts` allows one batch are spent, the
/// batch is discarded and the record describing it is written to the destination its
/// `DestinationConfig$OnFailure` names — for a BUCKET destination, as one object under the key
/// prefix that ARN states inside the bucket, whose payload is that record.
#[tokio::test]
async fn a_discarded_kinesis_batch_is_recorded_as_an_object_of_its_bucket_destination() {
    let router = emulator();
    let role_arn = given_role(&router).await;
    given_bucket(&router).await;
    let stream_arn = given_stream(&router).await;
    // The record goes in BEFORE the mapping exists, so the mapping's first read from
    // `TRIM_HORIZON` takes it.
    let payload = "poison-only";
    kinesis(
        &router,
        "PutRecord",
        &json!({
            "StreamName": STREAM,
            "PartitionKey": payload,
            "Data": ::aws_smithy_types::base64::encode(payload.as_bytes()),
        }),
    )
    .await;
    given_function(&router, &role_arn).await;
    let (uuid, function_arn) = given_mapping(&router, &stream_arn).await;

    let keys = awaited_keys(&router).await;
    assert_eq!(
        keys.len(),
        1,
        "exactly one object must be written per discarded batch, got {keys:?}"
    );
    let key = keys[0].clone();
    assert!(
        key.starts_with(&format!("{PREFIX}/")) && key.contains(&uuid),
        "the object must sit under the prefix the destination states and name the mapping \
         {uuid:?}, got {key:?}"
    );

    let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}")).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 reading {key:?} back, got {status} with {body}"
    );
    let record: Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("the record must be a JSON document, got {body}"));
    assert_eq!(
        record["version"],
        json!(RECORD_SCHEMA_VERSION),
        "the record must carry its schema version, got {record}"
    );
    assert_eq!(
        record["requestContext"]["functionArn"],
        json!(function_arn),
        "the record must name the function the batch was handed to, got {record}"
    );
    assert_eq!(
        record["requestContext"]["condition"],
        json!(CONDITION_RETRY_ATTEMPTS_EXHAUSTED),
        "the record must say the retries are what ran out, got {record}"
    );
    assert_eq!(
        record["requestContext"]["approximateInvokeCount"],
        json!(RETRIES + 1),
        "a mapping allowing {RETRIES} retries gives the batch {} attempts, got {record}",
        RETRIES + 1
    );
    assert_eq!(
        record["responseContext"]["functionError"],
        json!(UNHANDLED),
        "the record must report the error the last attempt reported, got {record}"
    );
    let info = &record[KINESIS_BATCH_INFO];
    assert_eq!(
        info["streamArn"],
        json!(stream_arn),
        "the batch must name the stream it was read from, got {record}"
    );
    assert_eq!(
        info["batchSize"],
        json!(1),
        "the batch must report how many records it held, got {record}"
    );
    assert!(
        info["shardId"]
            .as_str()
            .is_some_and(|shard| shard.starts_with("shardId-")),
        "the batch must name the shard it was read from, got {record}"
    );
    assert!(
        !body.contains(payload),
        "the record must describe the batch and carry no record payload, got {body}"
    );
}
