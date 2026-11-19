// SPDX-License-Identifier: CC0-1.0

//! A stack-deployed, Kinesis-sourced Firehose stream writes its destination only where the
//! destination's own role may make that write — and reports the refusal where the stream's
//! logging says.
//!
//! The rule under test: **Firehose makes a destination's object write under the role that
//! destination configuration names ("the Amazon Resource Name (ARN) of the Amazon Web
//! Services credentials" — `S3DestinationConfiguration$RoleARN`), so the write happens only
//! where the permissions in force on THAT role allow Amazon S3's own write of the object the
//! delivery would create — whichever way the stream was provisioned and whichever way its
//! records arrive. A delivery the role may not make writes nothing at all, and is reported as
//! one delivery-error event, whose `errorCode` is the fixed `S3.AccessDenied`, into the log
//! group and log stream the destination's `CloudWatchLoggingOptions` name ("The Amazon
//! CloudWatch logging options for your Firehose stream"); a delivery the role may make is
//! written and reports nothing.**
//!
//! So two streams of ONE stack, reading ONE Kinesis data stream and writing ONE bucket, differ
//! in what they deliver only by the role each names.
//!
//! Only a cross-service suite can ask this: the flow spans CloudFormation (which deploys the
//! roles and the streams), IAM (which holds those roles and decides each write), Amazon
//! Kinesis Data Streams (the source every stream here reads), Firehose (which buffers,
//! authorizes and delivers), Amazon S3 (which holds the bucket and the delivered objects) and
//! CloudWatch Logs (which holds the delivery-error events), mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them. The sibling suite
//! `cloudformation_firehose_stream_direct_put_delivers` pins the same authorization for a
//! `DirectPut` stream and reads no log event; the Firehose crate's own `delivery_role_refusal`
//! pins it for a stream a client created itself.
//!
//! The rows differ along what the rule generalises over: WHY the role may not make the write
//! (it states no object write at all, it states one scoped to another bucket, it states one
//! and then explicitly denies it), how the grant a permitted role carries is scoped (every
//! object of the bucket, only the objects under the destination's own key prefix), and which
//! member carries the Amazon S3 location (`S3DestinationConfiguration`, the
//! `ExtendedS3DestinationConfiguration` superset). Nothing is asserted about how fast the host
//! ran: every wait polls for the CONDITION under a generous deadline, and the destinations that
//! must stay empty are re-read at every one of those readings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The request builders the cross-service CloudFormation suites share, so this suite speaks the
// same wire as the ones beside it rather than building a second encoder of its own.
use crate::cloudformation_metric_stream_exports_to_s3::{cloudformation, send};
use crate::query_wire::{element, elements, outputs_of};

/// The one stack every row below is a resource of, so the suite spends ONE provisioning span.
const STACK: &str = "cfn-firehose-source-role";

/// How long the stack is given to report itself settled, and how long a stream is given to
/// report itself usable. The spans the services behind a stack choose take the time they take,
/// so the bound is a generous wall-clock deadline whose only job is that the wait TERMINATES.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(120);

/// How long a delivery, or the report of one that was refused, is waited for before a row
/// fails: far longer than the emulator's own source poll and buffer window together, so a
/// loaded machine does not fail a row the clock would have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How long the destinations that must hold NOTHING go on being re-read once every permitted
/// destination holds its object — spent re-reading them rather than sleeping once, so what the
/// assertions judge is the latest reading.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// How often a stack, a stream, a destination or a log stream is read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `StackStatus` a deployment the template declares correctly settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DeliveryStreamStatus` a stream takes and delivers data in.
const ACTIVE: &str = "ACTIVE";

/// The `errorCode` a destination whose role may not make the write reports — fixed text.
const REFUSED_ERROR_CODE: &str = "S3.AccessDenied";

/// The trust policy every row's role carries: Firehose assumes the role a stream's source and
/// destination name, so the service principal has to be able to.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The source read a `KinesisStreamAsSource` stream makes, which every row's role states so
/// that the ONE difference between the rows is what each may write.
const SOURCE_READ_STATEMENT: &str = concat!(
    r#"{"Effect":"Allow","Action":["kinesis:GetRecords","kinesis:GetShardIterator","#,
    r#""kinesis:DescribeStream"],"Resource":"*"}"#
);

/// Amazon S3's own write of an object, which is the action a delivery to an Amazon S3
/// destination is authorized under.
const OBJECT_WRITE: &str = "s3:PutObject";

/// What a row's role is allowed to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The object write, on every object of the destination's OWN bucket.
    EveryObjectOfTheBucket,
    /// The object write, on the objects under the destination's own key prefix only — a
    /// least-privilege grant that still covers the delivery.
    ObjectsUnderTheDestinationPrefix,
    /// No object write at all: the role states only the source read.
    NoObjectWrite,
    /// The object write, scoped to another bucket altogether — a least-privilege mistake, so
    /// the delivery to this row's destination is not a write this role may make.
    ObjectsOfAnotherBucket,
    /// The object write on the destination's own objects, and an explicit `Deny` of the same
    /// write: IAM's explicit deny wins, so the role may not make it.
    GrantedThenExplicitlyDenied,
}

/// What a row's destination holds once the source carries the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The record, as one delivered object under the declared prefix.
    Delivered,
    /// Nothing: the declared role may not make the write.
    Withheld,
}

/// One delivery stream the stack declares, reading the stack's one source and writing the
/// stack's one bucket under its own prefix.
struct Row {
    case: &'static str,
    /// The infix of this row's logical ids, its resource names and its key prefix.
    key: &'static str,
    /// The `CreateDeliveryStream` member the template declares the Amazon S3 destination under.
    member: &'static str,
    /// The `DeliveryStreamDescription` member Firehose reports that destination under.
    description: &'static str,
    grant: Grant,
    outcome: Outcome,
}

impl Row {
    /// The `Prefix` this row's destination declares, which no other row's destination shares.
    fn prefix(&self) -> String {
        format!("{}/", self.key)
    }

    /// The `DeliveryStreamName` the template declares, which is the resource's physical id.
    fn stream(&self) -> String {
        format!("{STACK}-{}-stream", self.key)
    }

    /// The IAM role the template declares this row's stream under.
    fn role(&self) -> String {
        format!("{STACK}-{}-role", self.key)
    }

    /// That role's ARN, which is the value the template hands to the stream's configuration.
    fn role_arn(&self) -> String {
        format!(
            "arn:aws:iam::{}:role/{}",
            engine::emulator_account_id(),
            self.role()
        )
    }

    /// The log stream this row's `CloudWatchLoggingOptions` name, one per row so that a report
    /// is attributable to the stream that made it.
    fn log_stream(&self) -> String {
        format!("{}-DestinationDelivery", self.key)
    }

    /// The logical id of one of this row's declared resources.
    fn logical_id(&self, kind: &str) -> String {
        let mut capitalised = self.key.to_uppercase();
        capitalised.truncate(1);
        capitalised.push_str(&self.key[1..]);
        format!("{kind}{capitalised}")
    }

    /// The identity policy the template puts on this row's role: the source read every row
    /// states, and the object write this row grants (or does not).
    fn role_policy(&self) -> String {
        let bucket = bucket_arn();
        let written = match self.grant {
            Grant::EveryObjectOfTheBucket | Grant::GrantedThenExplicitlyDenied => {
                format!("{bucket}/*")
            }
            Grant::ObjectsUnderTheDestinationPrefix => format!("{bucket}/{}*", self.prefix()),
            Grant::NoObjectWrite => String::new(),
            Grant::ObjectsOfAnotherBucket => format!("arn:aws:s3:::{STACK}-elsewhere/*"),
        };
        let mut statements = vec![SOURCE_READ_STATEMENT.to_string()];
        if !written.is_empty() {
            statements.push(format!(
                r#"{{"Effect":"Allow","Action":"{OBJECT_WRITE}","Resource":"{written}"}}"#
            ));
        }
        if self.grant == Grant::GrantedThenExplicitlyDenied {
            statements.push(format!(
                r#"{{"Effect":"Deny","Action":"{OBJECT_WRITE}","Resource":"{written}"}}"#
            ));
        }
        format!(
            r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
            statements.join(",")
        )
    }
}

/// The Kinesis data stream every row reads.
fn source_stream() -> String {
    format!("{STACK}-source")
}

/// The bucket every row writes, under its own prefix.
fn bucket() -> String {
    format!("{STACK}-data")
}

/// That bucket's ARN, which every row's destination names it by and which the error event of a
/// refused delivery reports as the destination it was going to.
fn bucket_arn() -> String {
    format!("arn:aws:s3:::{}", bucket())
}

/// The log group every row's `CloudWatchLoggingOptions` name.
fn log_group() -> String {
    format!("/aws/kinesisfirehose/{STACK}")
}

/// The record the producer writes to the shared source, which every row's stream retrieves for
/// itself.
fn marker() -> Vec<u8> {
    format!("{STACK}-marker\n").into_bytes()
}

/// The streams under test.
///
/// Two rows must deliver and three must not, so neither answer can be had by a store that
/// never delivers anything or by one that writes everything it is given.
const ROWS: &[Row] = &[
    Row {
        case: "a plain destination whose role may write every object of the bucket",
        key: "allowed",
        member: "S3DestinationConfiguration",
        description: "S3DestinationDescription",
        grant: Grant::EveryObjectOfTheBucket,
        outcome: Outcome::Delivered,
    },
    Row {
        case: "an extended destination whose role may write only its own key prefix",
        key: "scoped",
        member: "ExtendedS3DestinationConfiguration",
        description: "ExtendedS3DestinationDescription",
        grant: Grant::ObjectsUnderTheDestinationPrefix,
        outcome: Outcome::Delivered,
    },
    Row {
        case: "a plain destination whose role states no object write at all",
        key: "unstated",
        member: "S3DestinationConfiguration",
        description: "S3DestinationDescription",
        grant: Grant::NoObjectWrite,
        outcome: Outcome::Withheld,
    },
    Row {
        case: "an extended destination whose role may write only another bucket",
        key: "elsewhere",
        member: "ExtendedS3DestinationConfiguration",
        description: "ExtendedS3DestinationDescription",
        grant: Grant::ObjectsOfAnotherBucket,
        outcome: Outcome::Withheld,
    },
    Row {
        case: "a plain destination whose role grants the write and then denies it",
        key: "denied",
        member: "S3DestinationConfiguration",
        description: "S3DestinationDescription",
        grant: Grant::GrantedThenExplicitlyDenied,
        outcome: Outcome::Withheld,
    },
];

/// The stack's template: the one source, the one bucket, the one log group, and — per row — a
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
    let mut outputs = serde_json::Map::new();
    for row in ROWS {
        resources.insert(
            row.logical_id("Role"),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": row.role(),
                    "AssumeRolePolicyDocument": TRUST_POLICY,
                    "Policies": [{
                        "PolicyName": "delivery",
                        "PolicyDocument": row.role_policy(),
                    }],
                },
            }),
        );
        resources.insert(
            row.logical_id("LogStream"),
            json!({
                "Type": "AWS::Logs::LogStream",
                "Properties": {
                    // By reference, so the group holding the stream is created first.
                    "LogGroupName": { "Ref": "LogGroup" },
                    "LogStreamName": row.log_stream(),
                },
            }),
        );
        let destination = json!({
            "BucketARN": { "Fn::GetAtt": ["Bucket", "Arn"] },
            "RoleARN": { "Fn::GetAtt": [row.logical_id("Role"), "Arn"] },
            "CompressionFormat": "UNCOMPRESSED",
            "Prefix": row.prefix(),
            // The shortest wait `IntervalInSeconds` admits (published range 0..900) with the
            // smallest `SizeInMBs` (1..128): the rows are about WHETHER a record lands, not
            // about a window they would wait out.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
            "CloudWatchLoggingOptions": {
                "Enabled": true,
                "LogGroupName": log_group(),
                "LogStreamName": row.log_stream(),
            },
        });
        let mut stream = json!({
            "DeliveryStreamName": row.stream(),
            "DeliveryStreamType": "KinesisStreamAsSource",
            "KinesisStreamSourceConfiguration": {
                "KinesisStreamARN": { "Fn::GetAtt": ["Source", "Arn"] },
                "RoleARN": { "Fn::GetAtt": [row.logical_id("Role"), "Arn"] },
            },
        });
        stream[row.member] = destination;
        resources.insert(
            row.logical_id("Stream"),
            json!({
                "Type": "AWS::KinesisFirehose::DeliveryStream",
                "Properties": stream,
                // The destination reports into this stream, so the channel exists before the
                // stream that writes into it does.
                "DependsOn": [row.logical_id("LogStream")],
            }),
        );
        outputs.insert(
            row.logical_id("Stream"),
            json!({ "Value": { "Ref": row.logical_id("Stream") } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "Kinesis-sourced Firehose streams whose destination roles differ",
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
async fn a_deployed_streams_delivery_is_made_only_where_its_destination_role_allows_the_write() {
    let router = emulator();

    // ---- The stack: every row's role, log stream and delivery stream in one deployment. ----
    let body = template();
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // The template declares the IAM roles its streams deliver under, by name.
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
    let published = outputs_of(&described);

    // Every stream the stack deployed is the stream the template declared, carrying the role
    // and the prefix the template handed it: the provider changes neither.
    for row in ROWS {
        let name = published
            .get(&row.logical_id("Stream"))
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            name,
            row.stream(),
            "case {:?}: expected the stack to publish the declared stream name, got {name:?} in \
             {described}",
            row.case
        );
        let description = await_stream_active(&router, row).await;
        let destination = &description["Destinations"][0][row.description];
        assert_eq!(
            destination["RoleARN"],
            json!(row.role_arn()),
            "case {:?}: expected the destination to carry the declared RoleARN {:?}, got \
             {description}",
            row.case,
            row.role_arn()
        );
        assert_eq!(
            destination["Prefix"],
            json!(row.prefix()),
            "case {:?}: expected the destination to carry the declared Prefix {:?}, got \
             {description}",
            row.case,
            row.prefix()
        );
    }

    // ---- One record on the shared source, which every deployed stream retrieves for
    // itself. ----
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

    // ---- The destinations whose role allows the write hold the record; the others hold
    // nothing at every reading taken meanwhile. ----
    await_delivered(&router).await;

    // ---- And each refused delivery is reported where its stream's logging says, under the
    // one error code that condition carries. ----
    for row in ROWS {
        if row.outcome != Outcome::Withheld {
            continue;
        }
        await_reported(&router, row).await;
    }

    // The permitted destinations hold exactly the record, under their own declared prefix;
    // the refused ones still hold nothing, re-read throughout one more window, and report
    // nothing but that one error code.
    let until = Instant::now() + SILENCE_WINDOW;
    while Instant::now() < until {
        assert_holding_nothing(&router).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_holding_nothing(&router).await;

    for row in ROWS {
        let codes = reported_codes(&router, row).await;
        match row.outcome {
            Outcome::Delivered => {
                let keys = keys_under(&router, &row.prefix()).await;
                assert_eq!(
                    keys.len(),
                    1,
                    "case {:?}: one flushed buffer is one object under {:?}, got {keys:?}",
                    row.case,
                    row.prefix()
                );
                let held = object(&router, &keys[0]).await;
                assert!(
                    held == marker(),
                    "case {:?}: expected the object to hold the record the source carried \
                     ({:?}), got {:?}",
                    row.case,
                    String::from_utf8_lossy(&marker()),
                    String::from_utf8_lossy(&held)
                );
                assert!(
                    codes.is_empty(),
                    "case {:?}: a delivery the role allows reports nothing, got {codes:?}",
                    row.case
                );
            }
            Outcome::Withheld => {
                assert!(
                    !codes.is_empty()
                        && codes.iter().all(|code| code == REFUSED_ERROR_CODE),
                    "case {:?}: every reported delivery error is {REFUSED_ERROR_CODE:?}, got \
                     {codes:?}",
                    row.case
                );
            }
        }
    }
}

/// Poll the stack until it first reports a settled status, and answer the `DescribeStacks` body
/// that status was read from.
async fn await_settled(router: &MultiServiceRouter) -> String {
    let deadline = Instant::now() + DEPLOY_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
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

/// Poll Firehose until it reports `row`'s stream `ACTIVE`, and answer the
/// `DeliveryStreamDescription` it reported.
async fn await_stream_active(router: &MultiServiceRouter, row: &Row) -> Value {
    let deadline = Instant::now() + DEPLOY_DEADLINE;
    loop {
        let (status, body) = firehose(
            router,
            "DescribeDeliveryStream",
            &json!({ "DeliveryStreamName": row.stream() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from DescribeDeliveryStream, got {status} with {body}",
            row.case
        );
        let description = body["DeliveryStreamDescription"].clone();
        if description["DeliveryStreamStatus"] == json!(ACTIVE) {
            return description;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the stream to be {ACTIVE}, got {description}",
            row.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until every destination whose role allows the write holds its object, requiring every
/// destination that must hold nothing to be empty at each reading taken meanwhile.
async fn await_delivered(router: &MultiServiceRouter) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        assert_holding_nothing(router).await;
        let mut pending: Vec<&str> = Vec::new();
        for row in ROWS {
            if row.outcome == Outcome::Delivered
                && keys_under(router, &row.prefix()).await.is_empty()
            {
                pending.push(row.case);
            }
        }
        if pending.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected a delivered object for each of {pending:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Require every destination whose role may not make the write to be empty right now.
async fn assert_holding_nothing(router: &MultiServiceRouter) {
    for row in ROWS {
        if row.outcome != Outcome::Withheld {
            continue;
        }
        let keys = keys_under(router, &row.prefix()).await;
        assert!(
            keys.is_empty(),
            "case {:?}: nothing may be written under {:?}, got {keys:?}",
            row.case,
            row.prefix()
        );
    }
}

/// Wait until `row`'s log stream carries its delivery-error event, and require that event to
/// name the stream, the destination and the condition.
async fn await_reported(router: &MultiServiceRouter, row: &Row) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let events = reported_events(router, row).await;
        if let Some(document) = events.first() {
            assert_eq!(
                document["errorCode"],
                json!(REFUSED_ERROR_CODE),
                "case {:?}: expected the reported errorCode {REFUSED_ERROR_CODE:?}, got \
                 {document}",
                row.case
            );
            // The variable parts are the stream the records were put on and the destination
            // they were going to, both read off the deployed stream rather than fixed here.
            let stream_arn = document["deliveryStreamARN"].as_str().unwrap_or_default();
            assert!(
                stream_arn.starts_with("arn:aws:firehose:")
                    && stream_arn.ends_with(&format!(":deliverystream/{}", row.stream())),
                "case {:?}: expected the event to name its own stream, got {document}",
                row.case
            );
            assert_eq!(
                document["destination"],
                json!(bucket_arn()),
                "case {:?}: expected the event to name the destination it was going to, got \
                 {document}",
                row.case
            );
            assert!(
                document["message"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {:?}: expected the event to state what the code means, got {document}",
                row.case
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected a delivery-error event in {:?} / {:?}",
            row.case,
            log_group(),
            row.log_stream()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every delivery-error document `row`'s log stream holds, read through CloudWatch Logs' own
/// `GetLogEvents`.
async fn reported_events(router: &MultiServiceRouter, row: &Row) -> Vec<Value> {
    let (status, body) = json_rpc(
        router,
        "logs",
        "Logs_20140328.GetLogEvents",
        &json!({
            "logGroupName": log_group(),
            "logStreamName": row.log_stream(),
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

/// The `errorCode`s `row`'s log stream has reported so far.
async fn reported_codes(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    reported_events(router, row)
        .await
        .iter()
        .map(|document| {
            document["errorCode"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every key the shared bucket holds under `prefix`, read through Amazon S3's own
/// `ListObjectsV2`. Each row delivers at most one object, so one page holds them all.
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

/// One Firehose operation over its `awsJson1_1` wire.
async fn firehose(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    json_rpc(
        router,
        "firehose",
        &format!("Firehose_20150804.{operation}"),
        request,
    )
    .await
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
