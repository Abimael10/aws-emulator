// SPDX-License-Identifier: CC0-1.0

//! A stack-deployed, Kinesis-sourced Firehose stream whose role may not read the source: the
//! refusal is the only thing observable, and granting the missing permission in place
//! restores the retrieval.
//!
//! The rule under test: **Firehose retrieves a `KinesisStreamAsSource` stream's records under
//! the role its `KinesisStreamSourceConfiguration` names ("the ARN of the role that provides
//! access to the source stream" — `RoleARN`), so the retrieval happens only where the
//! permissions in force on THAT role allow the Kinesis Data Streams calls the read is made
//! of — whichever way the stream was provisioned. A read the role may not make retrieves
//! nothing, so nothing reaches the destination, and it is reported as a log event whose
//! `errorCode` is the fixed `Kinesis.AccessDenied` in the log group and log stream the
//! destination's `CloudWatchLoggingOptions` name; the permissions in force are read at each
//! attempt, so one granted while the stream exists restores the retrieval without touching
//! the stream.**
//!
//! So two streams of ONE stack, reading ONE Kinesis data stream and writing ONE bucket under
//! their own prefixes, differ in what they deliver only by what each one's role may read —
//! and the one that may not becomes the one that may the moment its policy says so, with the
//! records it never took still in the source.
//!
//! Only a cross-service suite can ask this: the flow spans CloudFormation (which deploys the
//! roles, the log destinations and the streams), IAM (which holds those roles and decides
//! each read), Amazon Kinesis Data Streams (the shared source), Firehose (which retrieves,
//! buffers and delivers), Amazon S3 (which holds the delivered objects) and CloudWatch Logs
//! (which holds the reported events), mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them. The sibling suite
//! `cloudformation_kinesis_sourced_firehose_delivery_role_authorization` pins the same
//! question for the DESTINATION write of such a stream, and
//! `firehose_kinesis_source_read_refusal` tables what each shape of source grant answers for
//! a stream a client created itself.
//!
//! Nothing here is asserted about how fast the host ran: every wait polls for the CONDITION
//! under a generous deadline, and what must stay empty is re-read at every one of those
//! readings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The request builders the cross-service CloudFormation suites share, so this suite speaks the
// same wire as the ones beside it rather than building a second encoder of its own.
use crate::cloudformation_metric_stream_exports_to_s3::{cloudformation, send};
use crate::query_wire::{element, elements, form_encode};

/// The one stack both streams below are resources of.
const STACK: &str = "cfn-firehose-source-permission";

/// How long the stack is given to report itself settled, and a stream to report itself usable.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(120);

/// How long a delivery, or the report of a read that was refused, is waited for: far longer
/// than the emulator's own source poll, authorization span and buffer window together.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How long what must hold NOTHING goes on being re-read once the condition holds.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// How often a stack, a stream, a destination or a log stream is read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `StackStatus` a deployment the template declares correctly settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DeliveryStreamStatus` a stream retrieves and delivers data in.
const ACTIVE: &str = "ACTIVE";

/// The `errorCode` a source read the stream's role may not make is reported under — fixed
/// text.
const REFUSED_ERROR_CODE: &str = "Kinesis.AccessDenied";

/// The trust policy both roles carry: Firehose assumes the role a stream's source and
/// destination name, so the service principal has to be able to.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The name of the one inline policy the template puts on each role, and the name the repair
/// re-states that policy under.
const POLICY_NAME: &str = "delivery";

/// The records read, which the refused role is the one short of.
const RECORDS_READ: &str = "kinesis:GetRecords";

/// The rest of the source read, which both roles state.
const OTHER_SOURCE_READS: &[&str] = &["kinesis:DescribeStream", "kinesis:GetShardIterator"];

/// Amazon S3's own write of an object, which both roles state so that the ONE difference
/// between the two streams is what each may read from the source.
const OBJECT_WRITE: &str = "s3:PutObject";

/// The Kinesis data stream both streams read.
fn source_stream() -> String {
    format!("{STACK}-source")
}

/// The bucket both streams write, each under its own prefix.
fn bucket() -> String {
    format!("{STACK}-data")
}

/// The log group both destinations report into.
fn log_group() -> String {
    format!("/aws/kinesisfirehose/{STACK}")
}

/// The record the producer writes to the shared source, which each stream retrieves for
/// itself.
fn marker() -> Vec<u8> {
    format!("{STACK}-marker\n").into_bytes()
}

/// One of the two streams the stack declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leg {
    /// The role may make the whole source read.
    Permitted,
    /// The role may make everything but the records read.
    Refused,
}

impl Leg {
    /// The infix of this leg's logical ids, resource names and key prefix.
    fn key(self) -> &'static str {
        match self {
            Self::Permitted => "good",
            Self::Refused => "bad",
        }
    }

    fn case(self) -> &'static str {
        match self {
            Self::Permitted => "a role that may read the source",
            Self::Refused => "a role one call short of the source read",
        }
    }

    /// The `Prefix` this leg's destination declares, which the other leg does not share.
    fn prefix(self) -> String {
        format!("{}/", self.key())
    }

    /// The `DeliveryStreamName` the template declares.
    fn stream(self) -> String {
        format!("{STACK}-{}-stream", self.key())
    }

    /// The IAM role the template declares this leg's stream under.
    fn role(self) -> String {
        format!("{STACK}-{}-role", self.key())
    }

    /// The log stream this leg's `CloudWatchLoggingOptions` name, one per leg so a report is
    /// attributable to the stream that made it.
    fn log_stream(self) -> String {
        format!("{}-DestinationDelivery", self.key())
    }

    /// The logical id of one of this leg's declared resources.
    fn logical_id(self, kind: &str) -> String {
        match self {
            Self::Permitted => format!("{kind}Good"),
            Self::Refused => format!("{kind}Bad"),
        }
    }

    /// The identity policy the template puts on this leg's role: the object write both state,
    /// and the source read — whole for the permitted leg, one call short for the refused one.
    fn role_policy(self) -> String {
        let mut reads: Vec<&str> = OTHER_SOURCE_READS.to_vec();
        if self == Self::Permitted {
            reads.push(RECORDS_READ);
        }
        policy_document(&reads)
    }
}

/// An identity policy stating the object write on the stack's bucket and `reads` on the
/// stack's source stream.
fn policy_document(reads: &[&str]) -> String {
    let actions = reads
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            r#"{{"Version":"2012-10-17","Statement":["#,
            r#"{{"Effect":"Allow","Action":[{}],"Resource":"*"}},"#,
            r#"{{"Effect":"Allow","Action":"{}","Resource":"arn:aws:s3:::{}/*"}}]}}"#
        ),
        actions,
        OBJECT_WRITE,
        bucket()
    )
}

/// The stack's template: the one source, the one bucket, the one log group, and — per leg — a
/// role, the log stream its destination reports into, and the Kinesis-sourced stream itself.
fn template() -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Source".to_string(),
        json!({
            "Type": "AWS::Kinesis::Stream",
            "Properties": { "Name": source_stream(), "ShardCount": 1 },
        }),
    );
    resources.insert(
        "Bucket".to_string(),
        json!({
            "Type": "AWS::S3::Bucket",
            "Properties": { "BucketName": bucket() },
        }),
    );
    resources.insert(
        "LogGroup".to_string(),
        json!({
            "Type": "AWS::Logs::LogGroup",
            "Properties": { "LogGroupName": log_group() },
        }),
    );
    for leg in [Leg::Permitted, Leg::Refused] {
        resources.insert(
            leg.logical_id("Role"),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": leg.role(),
                    "AssumeRolePolicyDocument": TRUST_POLICY,
                    "Policies": [{
                        "PolicyName": POLICY_NAME,
                        "PolicyDocument": leg.role_policy(),
                    }],
                },
            }),
        );
        resources.insert(
            leg.logical_id("LogStream"),
            json!({
                "Type": "AWS::Logs::LogStream",
                "Properties": {
                    // By reference, so the group holding the stream is created first.
                    "LogGroupName": { "Ref": "LogGroup" },
                    "LogStreamName": leg.log_stream(),
                },
            }),
        );
        resources.insert(
            leg.logical_id("Stream"),
            json!({
                "Type": "AWS::KinesisFirehose::DeliveryStream",
                "Properties": {
                    "DeliveryStreamName": leg.stream(),
                    "DeliveryStreamType": "KinesisStreamAsSource",
                    "KinesisStreamSourceConfiguration": {
                        "KinesisStreamARN": { "Fn::GetAtt": ["Source", "Arn"] },
                        "RoleARN": { "Fn::GetAtt": [leg.logical_id("Role"), "Arn"] },
                    },
                    "S3DestinationConfiguration": {
                        "BucketARN": { "Fn::GetAtt": ["Bucket", "Arn"] },
                        "RoleARN": { "Fn::GetAtt": [leg.logical_id("Role"), "Arn"] },
                        "CompressionFormat": "UNCOMPRESSED",
                        "Prefix": leg.prefix(),
                        // The shortest wait `IntervalInSeconds` admits (published range
                        // 0..900) with the smallest `SizeInMBs` (1..128): the legs are about
                        // WHETHER a record is retrieved, not about a window they would wait
                        // out.
                        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                        "CloudWatchLoggingOptions": {
                            "Enabled": true,
                            "LogGroupName": log_group(),
                            "LogStreamName": leg.log_stream(),
                        },
                    },
                },
                // The destination reports into that log stream, so the channel exists before
                // the stream writing into it does.
                "DependsOn": [leg.logical_id("LogStream")],
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "Kinesis-sourced Firehose streams whose source roles differ",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// The six services this flow spans, on ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's concern, not any of these services'.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
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
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

#[tokio::test]
async fn a_deployed_streams_retrieval_follows_the_permissions_its_source_role_carries() {
    let router = emulator();

    // ---- The stack: both roles, both log destinations and both delivery streams. ----
    let body = template();
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // The template declares the IAM roles its streams read and write under, by name.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let described = await_settled(&router).await;
    let reported = element(&described, "StackStatus");
    if reported != CREATE_COMPLETE {
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        let reasons = elements(&events, "ResourceStatusReason");
        panic!(
            "expected the stack to reach {CREATE_COMPLETE}, got {reported} with resource \
             failures {reasons:?}"
        );
    }
    for leg in [Leg::Permitted, Leg::Refused] {
        await_stream_active(&router, leg).await;
    }

    // ---- One record on the shared source, which each deployed stream retrieves for
    // itself — if its role lets it. ----
    let (status, answered) = json_rpc(
        &router,
        "kinesis",
        "Kinesis_20131202.PutRecord",
        &json!({
            "StreamName": source_stream(),
            "PartitionKey": "pk-marker",
            "Data": ::aws_smithy_types::base64::encode(marker()),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the source to accept the record, got {status} with {answered}"
    );

    // ---- The stream whose role may read delivers the record; the other delivers nothing at
    // every reading taken meanwhile. ----
    let keys = await_delivered(&router, Leg::Permitted).await;
    assert_eq!(
        keys.len(),
        1,
        "case {:?}: one retrieved record is one delivered object under {:?}, got {keys:?}",
        Leg::Permitted.case(),
        Leg::Permitted.prefix()
    );
    let held = object(&router, &keys[0]).await;
    assert!(
        held == marker(),
        "case {:?}: expected the object to hold the record the source carried ({:?}), got {:?}",
        Leg::Permitted.case(),
        String::from_utf8_lossy(&marker()),
        String::from_utf8_lossy(&held)
    );

    // ---- And the refused read is reported where that stream's logging says, under the one
    // error code that condition carries, naming the refused call, the source and the role. ----
    let document = await_reported(&router, Leg::Refused).await;
    assert_eq!(
        document["errorCode"],
        json!(REFUSED_ERROR_CODE),
        "case {:?}: expected the reported errorCode {REFUSED_ERROR_CODE:?}, got {document}",
        Leg::Refused.case()
    );
    let stream_arn = document["deliveryStreamARN"].as_str().unwrap_or_default();
    assert!(
        stream_arn.starts_with("arn:aws:firehose:")
            && stream_arn.ends_with(&format!(":deliverystream/{}", Leg::Refused.stream())),
        "case {:?}: expected the event to name its own stream, got {document}",
        Leg::Refused.case()
    );
    let message = document["message"].as_str().unwrap_or_default().to_string();
    for named in [RECORDS_READ, &source_stream(), &Leg::Refused.role()] {
        assert!(
            message.contains(named),
            "case {:?}: expected the message to name {named:?}, got {message:?}",
            Leg::Refused.case()
        );
    }
    // Nothing of a refused retrieval reaches the destination, and the permitted stream
    // reports nothing at all, at every reading over one more window.
    let until = Instant::now() + SILENCE_WINDOW;
    while Instant::now() < until {
        let keys = keys_under(&router, &Leg::Refused.prefix()).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a refused retrieval delivers nothing, got {keys:?}",
            Leg::Refused.case()
        );
        let reported = reported_events(&router, Leg::Permitted).await;
        assert!(
            reported.is_empty(),
            "case {:?}: a retrieval that was made reports nothing, got {reported:?}",
            Leg::Permitted.case()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // ---- The repair: the missing call is granted IN PLACE, on the role the deployed stream
    // already names. The record it never took is still in the source, so the retrieval it
    // could not make is made now and the destination holds the record. ----
    let mut reads: Vec<&str> = OTHER_SOURCE_READS.to_vec();
    reads.push(RECORDS_READ);
    let repaired = policy_document(&reads);
    let (status, answered) = iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", &Leg::Refused.role()),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &repaired),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from PutRolePolicy, got {status} with {answered}"
    );
    let keys = await_delivered(&router, Leg::Refused).await;
    let held = object(&router, &keys[0]).await;
    assert!(
        held == marker(),
        "case {:?}: expected the repaired stream to deliver the record the source carried \
         ({:?}), got {:?}",
        Leg::Refused.case(),
        String::from_utf8_lossy(&marker()),
        String::from_utf8_lossy(&held)
    );
}

/// Poll the stack until it first reports a settled status, and answer the `DescribeStacks`
/// body that status was read from.
async fn await_settled(router: &MultiServiceRouter) -> String {
    let deadline = Instant::now() + DEPLOY_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
        assert_eq!(
            status, 200,
            "expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reported = element(&body, "StackStatus");
        if reported.ends_with("_COMPLETE") || reported.ends_with("_FAILED") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "expected the stack to settle in {CREATE_COMPLETE}, last saw {reported:?} in {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll Firehose until it reports `leg`'s stream `ACTIVE`, reading the source the template
/// handed it.
async fn await_stream_active(router: &MultiServiceRouter, leg: Leg) {
    let deadline = Instant::now() + DEPLOY_DEADLINE;
    loop {
        let (status, body) = json_rpc(
            router,
            "firehose",
            "Firehose_20150804.DescribeDeliveryStream",
            &json!({ "DeliveryStreamName": leg.stream() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from DescribeDeliveryStream, got {status} with {body}",
            leg.case()
        );
        let description = &body["DeliveryStreamDescription"];
        if description["DeliveryStreamStatus"] == json!(ACTIVE) {
            assert!(
                description["Source"]["KinesisStreamSourceDescription"]["KinesisStreamARN"]
                    .as_str()
                    .is_some_and(|arn| arn.ends_with(&format!("stream/{}", source_stream()))),
                "case {:?}: expected the stream to report the source it reads, got {description}",
                leg.case()
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the stream to be {ACTIVE}, got {description}",
            leg.case()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `leg`'s destination holds at least one object, and answer its keys.
async fn await_delivered(router: &MultiServiceRouter, leg: Leg) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let keys = keys_under(router, &leg.prefix()).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected a delivered object under {:?} within {DELIVERY_DEADLINE:?}",
            leg.case(),
            leg.prefix()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `leg`'s log stream carries its first reported event, and answer that document.
async fn await_reported(router: &MultiServiceRouter, leg: Leg) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        if let Some(document) = reported_events(router, leg).await.into_iter().next() {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected a reported event in {:?} / {:?} within {DELIVERY_DEADLINE:?}",
            leg.case(),
            log_group(),
            leg.log_stream()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every document `leg`'s log stream holds, read through CloudWatch Logs' own `GetLogEvents`.
async fn reported_events(router: &MultiServiceRouter, leg: Leg) -> Vec<Value> {
    let (status, body) = json_rpc(
        router,
        "logs",
        "Logs_20140328.GetLogEvents",
        &json!({
            "logGroupName": log_group(),
            "logStreamName": leg.log_stream(),
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str())
        .filter_map(|message| serde_json::from_str::<Value>(message).ok())
        .collect()
}

/// Every key the shared bucket holds under `prefix`, read through Amazon S3's own
/// `ListObjectsV2`. Each leg delivers at most one object, so one page holds them all.
async fn keys_under(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{}?list-type=2&prefix={prefix}", bucket()),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status,
        200,
        "expected HTTP 200 from ListObjectsV2 on {} under {prefix:?}, got {status} with {text}",
        bucket()
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// The bytes one delivered object holds, read through `GetObject`.
async fn object(router: &MultiServiceRouter, key: &str) -> Vec<u8> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{}/{key}", bucket()),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "expected HTTP 200 from GetObject for {key:?}, got {status} with {}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// One `awsJson1_1` operation of `signing_name`, answering its status and parsed body.
async fn json_rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    request: &Value,
) -> (u16, Value) {
    let (status, body) = send(
        router,
        signing_name,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target),
        ],
        request.to_string().into_bytes(),
    )
    .await;
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One IAM action over its `awsQuery` wire, answering its status and body text.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
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
    (status, String::from_utf8_lossy(&answered).to_string())
}
