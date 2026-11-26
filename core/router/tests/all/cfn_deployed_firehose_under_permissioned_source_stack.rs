// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares Kinesis-sourced Firehose streams whose source roles may
//! NOT make the retrieval: the stack deploys, and the refusal is a condition of the live stream.
//!
//! The rule under test: **a Firehose stream's control-plane create resolves what its
//! configuration NAMES — the resources it points at and the roles it is assumed under — and not
//! what those roles are ALLOWED to do, because the permissions that decide a call made under a
//! role are the ones in force at the ATTEMPT. So `CreateDeliveryStream` is served for a
//! `KinesisStreamAsSource` stream whose `KinesisStreamSourceConfiguration` names a role whose
//! policy withholds the Kinesis Data Streams calls the retrieval is made of
//! (`kinesis:DescribeStream`, `kinesis:GetShardIterator`, `kinesis:GetRecords`), the stream
//! becomes `ACTIVE`, and the refusal surfaces where a live stream's conditions surface: one
//! event under the fixed `errorCode` `Kinesis.AccessDenied` in the log group and log stream the
//! destination's `CloudWatchLoggingOptions` name, naming the first call the role may not make.
//! A CloudFormation resource therefore CREATES and stabilizes for such a stream, and a stack
//! declaring one reaches `CREATE_COMPLETE` with its `Outputs` resolved rather than being rolled
//! back.**
//!
//! Only a cross-service suite can ask this: the flow spans CloudFormation (which deploys the
//! roles, the log destinations and the streams), IAM (which holds those roles and decides each
//! read), Amazon Kinesis Data Streams (the shared source), Firehose (which retrieves and
//! delivers), Amazon S3 (which holds the delivered objects) and CloudWatch Logs (which holds
//! the reported events), mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! The rows are legs of ONE stack, declared with the same source, bucket and log group and
//! differing only in what their role's policy says about the source: one role that may make the
//! whole read, and — along what the rule generalises over, WHICH of the three calls is withheld
//! and HOW — a role missing the records read, one missing the shard iterator, one missing the
//! describe, one carrying no Kinesis grant at all, and one whose grant names another stream.
//! Every leg has to deploy, and the one that may read has to deliver, so no leg can pass by a
//! stack that creates nothing.
//!
//! The sibling suite `cfn_deployed_firehose_source_permission_repair` asks the same question
//! from the other end — a grant taken away and restored while the streams are live — and
//! `firehose_create_resolves_its_configuration` tables what a create DOES resolve: the
//! existence of the Amazon S3 locations a destination names.
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
use crate::query_wire::{element, elements, form_encode, outputs_of};

/// The one stack every leg below is a resource of.
const STACK: &str = "cfn-firehose-under-permissioned-source";

/// How long the stack is given to report itself settled, and a stream to report itself usable.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(120);

/// How long a delivery, or the report of a read that was refused, is waited for: far longer
/// than the emulator's own source poll, authorization span and buffer window together.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How long what must hold NOTHING goes on being re-read once every expected condition holds.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// How often a stack, a stream, a destination or a log stream is read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `StackStatus` a deployment of a template like this settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DeliveryStreamStatus` a stream retrieves and delivers data in.
const ACTIVE: &str = "ACTIVE";

/// The `errorCode` a source read the stream's role may not make is reported under — fixed text.
const REFUSED_ERROR_CODE: &str = "Kinesis.AccessDenied";

/// The trust policy every role carries: Firehose assumes the role a stream's source and
/// destination name, so the service principal has to be able to.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The name of the one inline policy the template puts on each role.
const POLICY_NAME: &str = "delivery";

/// Amazon S3's own write of an object, which every role states so that the ONE difference
/// between the legs is what each may read from the source.
const OBJECT_WRITE: &str = "s3:PutObject";

/// The three Kinesis Data Streams calls the retrieval from a source stream is made of, in the
/// order the retrieval makes them — so the FIRST one a policy withholds is the one its leg's
/// report names.
const DESCRIBE: &str = "kinesis:DescribeStream";
const SHARD_ITERATOR: &str = "kinesis:GetShardIterator";
const RECORDS: &str = "kinesis:GetRecords";
const EVERY_SOURCE_READ: &[&str] = &[DESCRIBE, SHARD_ITERATOR, RECORDS];

/// The retention the log group declares, in days — the shortest the member admits.
const RETENTION_IN_DAYS: u32 = 1;

/// The Kinesis data stream every leg reads.
fn source_stream() -> String {
    format!("{STACK}-source")
}

/// The bucket every leg writes, each under its own prefix.
fn bucket() -> String {
    format!("{STACK}-data")
}

/// The log group every destination reports into.
fn log_group() -> String {
    format!("/aws/kinesisfirehose/{STACK}")
}

/// The record the producer writes to the shared source, which each leg retrieves for itself —
/// if its role lets it.
fn marker() -> Vec<u8> {
    format!("{STACK}-marker\n").into_bytes()
}

/// One leg of the stack: a role, the log stream its destination reports into, and the
/// Kinesis-sourced delivery stream that reads under that role.
struct Leg {
    case: &'static str,
    /// The infix of this leg's logical ids and the start of its output names.
    id: &'static str,
    /// The infix of this leg's resource names and its key prefix.
    key: &'static str,
    /// Which of the source read calls this leg's role allows; empty for a role carrying no
    /// Kinesis grant at all.
    granted: &'static [&'static str],
    /// Whether the grant it carries names the stack's own source (`true`) or another stream.
    scoped_to_the_source: bool,
    /// The first call of [`EVERY_SOURCE_READ`] the role may not make, which its report names;
    /// `None` for the leg whose role may make the whole read.
    refused: Option<&'static str>,
}

impl Leg {
    /// The `Prefix` this leg's destination declares, which no other leg shares.
    fn prefix(&self) -> String {
        format!("{}/", self.key)
    }

    /// The `DeliveryStreamName` the template declares.
    fn stream(&self) -> String {
        format!("{STACK}-{}-stream", self.key)
    }

    /// The IAM role the template declares this leg's stream under.
    fn role(&self) -> String {
        format!("{STACK}-{}-role", self.key)
    }

    /// The log stream this leg's `CloudWatchLoggingOptions` name, one per leg so a report is
    /// attributable to the stream that made it.
    fn log_stream(&self) -> String {
        format!("{}-DestinationDelivery", self.key)
    }

    /// The logical id of one of this leg's declared resources.
    fn logical_id(&self, kind: &str) -> String {
        format!("{kind}{}", self.id)
    }

    /// The name of one of this leg's stack outputs.
    fn output(&self, of: &str) -> String {
        format!("{}{of}", self.id)
    }
}

/// The legs: one role that may make the whole read, and one per shape of policy that withholds
/// part of it.
const LEGS: &[Leg] = &[
    Leg {
        case: "a role that may make the whole source read",
        id: "Good",
        key: "good",
        granted: EVERY_SOURCE_READ,
        scoped_to_the_source: true,
        refused: None,
    },
    Leg {
        case: "a role one call short of the source read",
        id: "NoRecords",
        key: "norecords",
        granted: &[DESCRIBE, SHARD_ITERATOR],
        scoped_to_the_source: true,
        refused: Some(RECORDS),
    },
    Leg {
        case: "a role that may not take a position in the shards",
        id: "NoIterator",
        key: "noiterator",
        granted: &[DESCRIBE, RECORDS],
        scoped_to_the_source: true,
        refused: Some(SHARD_ITERATOR),
    },
    Leg {
        case: "a role that may not describe the source",
        id: "NoDescribe",
        key: "nodescribe",
        granted: &[SHARD_ITERATOR, RECORDS],
        scoped_to_the_source: true,
        refused: Some(DESCRIBE),
    },
    Leg {
        case: "a role carrying no Kinesis grant at all",
        id: "NoGrant",
        key: "nogrant",
        granted: &[],
        scoped_to_the_source: true,
        refused: Some(DESCRIBE),
    },
    Leg {
        case: "a role whose source grant names another stream",
        id: "Elsewhere",
        key: "elsewhere",
        granted: EVERY_SOURCE_READ,
        scoped_to_the_source: false,
        refused: Some(DESCRIBE),
    },
];

/// The identity policy the template puts on one leg's role: the object write every leg makes,
/// and this leg's own source grant.
///
/// The source grant is written against the source by `Fn::GetAtt`, so a leg scoped to the
/// stack's own stream names the very stream its delivery stream reads; a leg scoped elsewhere
/// names a stream of this account that nothing declares.
fn policy_document(leg: &Leg) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": OBJECT_WRITE,
        "Resource": { "Fn::Sub": format!("${{Bucket.Arn}}/{}*", leg.prefix()) },
    })];
    if !leg.granted.is_empty() {
        let resource = if leg.scoped_to_the_source {
            json!({ "Fn::GetAtt": ["Source", "Arn"] })
        } else {
            json!({ "Fn::Sub": "arn:${AWS::Partition}:kinesis:${AWS::Region}:${AWS::AccountId}:stream/nothing-declares-this" })
        };
        statements.push(json!({
            "Effect": "Allow",
            "Action": leg.granted,
            "Resource": resource,
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
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
            "Properties": { "LogGroupName": log_group(), "RetentionInDays": RETENTION_IN_DAYS },
        }),
    );
    let mut outputs = serde_json::Map::new();
    outputs.insert("BucketName".to_string(), json!({ "Value": { "Ref": "Bucket" } }));
    outputs.insert("StreamName".to_string(), json!({ "Value": { "Ref": "Source" } }));
    outputs.insert(
        "LogGroupName".to_string(),
        json!({ "Value": { "Ref": "LogGroup" } }),
    );
    for leg in LEGS {
        resources.insert(
            leg.logical_id("Role"),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": leg.role(),
                    "AssumeRolePolicyDocument": TRUST_POLICY,
                    "Policies": [{
                        "PolicyName": POLICY_NAME,
                        "PolicyDocument": policy_document(leg),
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
                    // The deprecated spelling of an Amazon S3 destination, which the model
                    // still accepts.
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
                            "LogGroupName": { "Ref": "LogGroup" },
                            "LogStreamName": { "Ref": leg.logical_id("LogStream") },
                        },
                    },
                },
                // The destination reports into that log stream, so the channel exists before
                // the stream writing into it does.
                "DependsOn": [leg.logical_id("LogStream")],
            }),
        );
        outputs.insert(
            leg.output("LogStreamName"),
            json!({ "Value": { "Ref": leg.logical_id("LogStream") } }),
        );
        outputs.insert(
            leg.output("RoleName"),
            json!({ "Value": { "Ref": leg.logical_id("Role") } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "Kinesis-sourced Firehose streams whose source roles may not read",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
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
async fn a_stack_whose_firehose_source_roles_may_not_read_deploys_and_reports_the_refusal() {
    let router = emulator();

    // ---- The stack: every leg's role, log destination and delivery stream. ----
    let body = template();
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // The template declares the IAM roles its streams read and write under, by name.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );

    // The stack settles in CREATE_COMPLETE: a resource whose role may not read is still a
    // resource that CREATES and stabilizes, so nothing here is rolled back.
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

    // And its Outputs are there to be read, which a rolled-back stack has none of.
    let outputs = outputs_of(&described);
    let mut expected_outputs = vec![
        ("BucketName".to_string(), bucket()),
        ("StreamName".to_string(), source_stream()),
        ("LogGroupName".to_string(), log_group()),
    ];
    for leg in LEGS {
        expected_outputs.push((leg.output("LogStreamName"), leg.log_stream()));
        expected_outputs.push((leg.output("RoleName"), leg.role()));
    }
    for (key, value) in expected_outputs {
        assert_eq!(
            outputs.get(&key).map(String::as_str),
            Some(value.as_str()),
            "expected the output {key:?} to resolve to {value:?}, got {outputs:?}"
        );
    }

    // Every leg's stream is usable and reports the source it was configured with, whatever its
    // role's policy withholds.
    for leg in LEGS {
        await_stream_active(&router, leg).await;
    }

    // ---- One record on the shared source, which each deployed stream retrieves for itself —
    // if its role lets it. ----
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

    // ---- The leg whose role may read delivers the record. ----
    let permitted = LEGS
        .iter()
        .find(|leg| leg.refused.is_none())
        .expect("one leg's role may make the whole read");
    let keys = await_delivered(&router, permitted).await;
    assert_eq!(
        keys.len(),
        1,
        "case {:?}: one retrieved record is one delivered object under {:?}, got {keys:?}",
        permitted.case,
        permitted.prefix()
    );
    let held = object(&router, &keys[0]).await;
    assert!(
        held == marker(),
        "case {:?}: expected the object to hold the record the source carried ({:?}), got {:?}",
        permitted.case,
        String::from_utf8_lossy(&marker()),
        String::from_utf8_lossy(&held)
    );

    // ---- And every leg whose role may not read reports the refusal where its destination's
    // logging says, under the one error code that condition carries, naming the FIRST call the
    // role may not make, the source stream and the role. ----
    for leg in LEGS.iter().filter(|leg| leg.refused.is_some()) {
        let action = leg.refused.unwrap_or_default();
        let document = await_reported(&router, leg).await;
        assert_eq!(
            document["errorCode"],
            json!(REFUSED_ERROR_CODE),
            "case {:?}: expected the reported errorCode {REFUSED_ERROR_CODE:?}, got {document}",
            leg.case
        );
        let stream_arn = document["deliveryStreamARN"].as_str().unwrap_or_default();
        assert!(
            stream_arn.starts_with("arn:aws:firehose:")
                && stream_arn.ends_with(&format!(":deliverystream/{}", leg.stream())),
            "case {:?}: expected the event to name its own stream, got {document}",
            leg.case
        );
        let message = document["message"].as_str().unwrap_or_default().to_string();
        for named in [action, &source_stream(), &leg.role()] {
            assert!(
                message.contains(named),
                "case {:?}: expected the message to name {named:?}, got {message:?}",
                leg.case
            );
        }
    }

    // ---- A refused retrieval delivers nothing, and the leg that did retrieve reports nothing
    // at all, at every reading over one more window. ----
    let until = Instant::now() + SILENCE_WINDOW;
    while Instant::now() < until {
        for leg in LEGS.iter().filter(|leg| leg.refused.is_some()) {
            let keys = keys_under(&router, &leg.prefix()).await;
            assert!(
                keys.is_empty(),
                "case {:?}: a refused retrieval delivers nothing, got {keys:?}",
                leg.case
            );
        }
        let reported = reported_events(&router, permitted).await;
        assert!(
            reported.is_empty(),
            "case {:?}: a retrieval that was made reports nothing, got {reported:?}",
            permitted.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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

/// Poll Firehose until it reports `leg`'s stream `ACTIVE`, reading the source and the role the
/// template handed it.
async fn await_stream_active(router: &MultiServiceRouter, leg: &Leg) {
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
            leg.case
        );
        let description = &body["DeliveryStreamDescription"];
        if description["DeliveryStreamStatus"] == json!(ACTIVE) {
            let source = &description["Source"]["KinesisStreamSourceDescription"];
            assert!(
                source["KinesisStreamARN"]
                    .as_str()
                    .is_some_and(|arn| arn.ends_with(&format!("stream/{}", source_stream()))),
                "case {:?}: expected the stream to report the source it reads, got {description}",
                leg.case
            );
            assert!(
                source["RoleARN"]
                    .as_str()
                    .is_some_and(|arn| arn.ends_with(&format!("role/{}", leg.role()))),
                "case {:?}: expected the stream to report the role it reads under, got \
                 {description}",
                leg.case
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the stream to be {ACTIVE}, got {description}",
            leg.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `leg`'s destination holds at least one object, and answer its keys.
async fn await_delivered(router: &MultiServiceRouter, leg: &Leg) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let keys = keys_under(router, &leg.prefix()).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected a delivered object under {:?} within {DELIVERY_DEADLINE:?}",
            leg.case,
            leg.prefix()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `leg`'s log stream carries its first reported event, and answer that document.
async fn await_reported(router: &MultiServiceRouter, leg: &Leg) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        if let Some(document) = reported_events(router, leg).await.into_iter().next() {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected a reported event in {:?} / {:?} within {DELIVERY_DEADLINE:?}",
            leg.case,
            log_group(),
            leg.log_stream()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every document `leg`'s log stream holds, read through CloudWatch Logs' own `GetLogEvents`.
async fn reported_events(router: &MultiServiceRouter, leg: &Leg) -> Vec<Value> {
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
        &format!(
            "/{}?list-type=2&prefix={}",
            bucket(),
            form_encode(prefix)
        ),
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
