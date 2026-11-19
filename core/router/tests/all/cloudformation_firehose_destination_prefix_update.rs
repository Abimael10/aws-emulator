// SPDX-License-Identifier: CC0-1.0

//! An `UpdateStack` that states a deployed Firehose stream's Amazon S3 destination
//! DIFFERENTLY: the stack completes, the stream keeps its identity, and the records that
//! follow land where the new declaration says.
//!
//! The rule under test: **a destination configuration is not fixed when a Firehose stream is
//! created — "Updates the specified destination of the specified Firehose stream … The target
//! Firehose stream remains active while the configurations are updated"
//! (`UpdateDestination`) — so a CloudFormation declaration that states the destination
//! differently is applied IN PLACE to the stream the stack already has: the stream keeps the
//! name that is the resource's physical id, its ARN and the source it reads, Amazon Data
//! Firehose reports the declared configuration and a new `DeliveryStreamVersionId` ("After
//! the update is applied successfully, the version ID is updated"), and the stream's own data
//! path writes the records that follow under the configuration now in force.**
//!
//! Only a cross-service suite can ask this: the flow spans CloudFormation (which deploys the
//! change), IAM (which holds the role the delivery is made under), Amazon Kinesis Data
//! Streams (the source a `KinesisStreamAsSource` stream reads), Firehose (which applies the
//! change and delivers) and Amazon S3 (which holds the objects), mounted on ONE engine
//! exactly as `core/router/src/lib.rs` mounts them. The sibling suite
//! `cloudformation_firehose_stream_direct_put_delivers` pins what a DEPLOYED destination
//! does; this one pins what a CHANGED one does.
//!
//! The rows differ along what the rule generalises over: which member the destination is
//! declared under (the deprecated `S3DestinationConfiguration`, the
//! `ExtendedS3DestinationConfiguration` superset), how many members of the destination the
//! new declaration moves (the key `Prefix` alone, the prefix and the error-output prefix
//! together), and where the stream's records come from (a Kinesis data stream the template
//! declares, a direct `PutRecord` on the stream itself). Nothing is asserted about how fast
//! the host ran: every wait polls for the CONDITION under a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The request builders the cross-service CloudFormation suites share, so this suite speaks
// the same wire as the ones beside it rather than building a second encoder of its own.
use crate::cloudformation_metric_stream_exports_to_s3::{cloudformation, send};
use crate::query_wire::{element, elements, outputs_of};

/// How long a stack is given to report itself settled, and how often it is asked. The spans
/// the services behind a stack choose take the time they take, so the bound is a generous
/// wall-clock deadline whose only job is that the wait TERMINATES.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(120);

/// How long a delivery that IS due is waited for before a row fails: far longer than the
/// emulator's own source poll and buffer window together, so a loaded machine does not fail a
/// row the clock would have served.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How often a stack, or a destination, is read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The statuses a deployment settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The `DeliveryStreamStatus` a stream takes and delivers data in.
const ACTIVE: &str = "ACTIVE";

/// The template parameter every row's destination reads its prefix from, which is the one
/// value an update of these stacks states differently.
const PREFIX_PARAMETER: &str = "S3Prefix";

/// The trust policy every row's delivery role carries: Firehose assumes the role a stream's
/// source and destination name, so the service principal has to be able to.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// What that role is allowed: the source read a `KinesisStreamAsSource` stream makes, and the
/// object write its destination makes.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["kinesis:GetRecords","kinesis:GetShardIterator","kinesis:DescribeStream",""#,
    r#"s3:PutObject"],"Resource":"*"}]}"#
);

/// Where a row's records come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The Kinesis data stream the template declares, which the Firehose stream names in its
    /// `KinesisStreamSourceConfiguration`.
    KinesisStream,
    /// The stream itself: a client's own `PutRecord` on a `DirectPut` stream.
    DirectPut,
}

/// One deployed stream whose destination an update moves.
struct Row {
    case: &'static str,
    /// The stack this row deploys, which every other name it uses is derived from.
    stack: &'static str,
    /// The `CreateDeliveryStream` member the template declares the destination under.
    member: &'static str,
    /// The `DeliveryStreamDescription` member Firehose reports that destination under.
    description: &'static str,
    /// Whether the destination also declares an `ErrorOutputPrefix` derived from the same
    /// parameter, so the update moves TWO members of it.
    error_prefix: bool,
    source: Source,
    /// The prefix the create declares, and the one the update declares. Disjoint, so an
    /// object under one is never under the other.
    before: &'static str,
    after: &'static str,
}

impl Row {
    /// The bucket the template declares, and the one its objects are read back from.
    fn bucket(&self) -> String {
        format!("{}-data", self.stack)
    }

    /// The delivery stream the template names, which is the resource's physical id.
    fn stream(&self) -> String {
        format!("{}-stream", self.stack)
    }

    /// The Kinesis data stream the template declares as the row's source.
    fn source_stream(&self) -> String {
        format!("{}-source", self.stack)
    }

    /// The `ErrorOutputPrefix` the destination carries for `prefix`, where it declares one.
    fn error_output_prefix(&self, prefix: &str) -> String {
        format!("errors/{prefix}")
    }

    /// The record a client writes before the update, and the one it writes after it.
    fn first_marker(&self) -> Vec<u8> {
        format!("{}-before-marker\n", self.stack).into_bytes()
    }
    fn second_marker(&self) -> Vec<u8> {
        format!("{}-after-marker\n", self.stack).into_bytes()
    }

    /// The row's template: the source stream, the destination bucket, the delivery role and
    /// the Firehose stream delivering into it, with the destination's prefix read off the
    /// stack's own parameter so an update states only that parameter differently.
    fn template(&self) -> String {
        let mut destination = json!({
            "BucketARN": { "Fn::GetAtt": ["Bucket", "Arn"] },
            "RoleARN": { "Fn::GetAtt": ["Role", "Arn"] },
            "CompressionFormat": "UNCOMPRESSED",
            // The shortest wait `IntervalInSeconds` admits (published range 0..900) with the
            // smallest `SizeInMBs` (1..128): the rows are about WHERE a record lands, not
            // about a window they would wait out.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
            "Prefix": { "Ref": PREFIX_PARAMETER },
        });
        if self.error_prefix {
            destination["ErrorOutputPrefix"] =
                json!({ "Fn::Sub": format!("errors/${{{PREFIX_PARAMETER}}}") });
        }
        let mut stream = json!({ "DeliveryStreamName": self.stream() });
        stream[self.member] = destination;
        match self.source {
            Source::KinesisStream => {
                stream["DeliveryStreamType"] = json!("KinesisStreamAsSource");
                stream["KinesisStreamSourceConfiguration"] = json!({
                    "KinesisStreamARN": { "Fn::GetAtt": ["Source", "Arn"] },
                    "RoleARN": { "Fn::GetAtt": ["Role", "Arn"] },
                });
            }
            Source::DirectPut => stream["DeliveryStreamType"] = json!("DirectPut"),
        }
        // The parameter's NAME is a constant this suite also sends on the wire, so the
        // declaration is built rather than written as a literal key.
        let mut parameters = serde_json::Map::new();
        parameters.insert(
            PREFIX_PARAMETER.to_string(),
            json!({ "Type": "String", "Default": self.before }),
        );
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Description": "a Firehose stream whose destination prefix a parameter decides",
            "Parameters": Value::Object(parameters),
            "Resources": {
                "Source": {
                    "Type": "AWS::Kinesis::Stream",
                    "Properties": { "Name": self.source_stream(), "ShardCount": 1 },
                },
                "Bucket": {
                    "Type": "AWS::S3::Bucket",
                    "Properties": { "BucketName": self.bucket() },
                },
                "Role": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "RoleName": format!("{}-role", self.stack),
                        "AssumeRolePolicyDocument": TRUST_POLICY,
                        "Policies": [{
                            "PolicyName": "delivery",
                            "PolicyDocument": ROLE_POLICY,
                        }],
                    },
                },
                "Stream": {
                    "Type": "AWS::KinesisFirehose::DeliveryStream",
                    "Properties": stream,
                },
            },
            "Outputs": {
                "StreamName": { "Value": { "Ref": "Stream" } },
            },
        })
        .to_string()
    }
}

/// The deployed streams under test.
const ROWS: &[Row] = &[
    Row {
        case: "a source-fed stream whose plain S3 destination moves to another prefix",
        stack: "cfn-firehose-move-plain",
        member: "S3DestinationConfiguration",
        description: "S3DestinationDescription",
        error_prefix: false,
        source: Source::KinesisStream,
        before: "landing/",
        after: "second/",
    },
    Row {
        case: "a source-fed extended destination whose prefix and error prefix both move",
        stack: "cfn-firehose-move-extended",
        member: "ExtendedS3DestinationConfiguration",
        description: "ExtendedS3DestinationDescription",
        error_prefix: true,
        source: Source::KinesisStream,
        before: "raw/",
        after: "curated/nested/",
    },
    Row {
        case: "a direct-put stream whose extended destination moves to another prefix",
        stack: "cfn-firehose-move-direct",
        member: "ExtendedS3DestinationConfiguration",
        description: "ExtendedS3DestinationDescription",
        error_prefix: false,
        source: Source::DirectPut,
        before: "direct/",
        after: "moved/",
    },
];

/// The five services this flow spans, on ONE shared engine.
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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

#[tokio::test]
async fn an_updated_destination_is_applied_in_place_and_decides_where_records_land() {
    let router = emulator();

    // ---- Every stack is deployed before any of them is awaited, so the suite spends ONE
    // provisioning span rather than one per row. ----
    for row in ROWS {
        deploy(&router, row, "CreateStack", row.before).await;
    }
    for row in ROWS {
        let described = await_settled(&router, row, CREATE_COMPLETE).await;
        let published = outputs_of(&described)
            .get("StreamName")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            published,
            row.stream(),
            "case {:?}: expected the stack to publish the declared stream name, got \
             {published:?} in {described}",
            row.case
        );
    }

    // ---- A record put BEFORE the change lands under the prefix the create declared. ----
    let mut version_before: Vec<String> = Vec::new();
    for row in ROWS {
        let described = await_stream_active(&router, row).await;
        version_before.push(version_of(row, &described));
        assert_destination(row, &described, row.before);
        put_marker(&router, row, &row.first_marker()).await;
    }
    for row in ROWS {
        let key = await_marker(&router, row, row.before, &row.first_marker()).await;
        assert!(
            key.starts_with(row.before),
            "case {:?}: expected the delivered key to begin with the declared prefix {:?}, got \
             {key:?}",
            row.case,
            row.before
        );
    }

    // ---- The update: the same template, with only the prefix parameter stated differently.
    for row in ROWS {
        deploy(&router, row, "UpdateStack", row.after).await;
    }
    for (row, before) in ROWS.iter().zip(&version_before) {
        let described = await_settled(&router, row, UPDATE_COMPLETE).await;
        let status = element(&described, "StackStatus");
        if status != UPDATE_COMPLETE {
            let (_, events) =
                cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
            let reasons = elements(&events, "ResourceStatusReason");
            panic!(
                "case {:?}: expected the stack to reach {UPDATE_COMPLETE}, got {status} with \
                 resource failures {reasons:?}",
                row.case
            );
        }
        // The stream the stack publishes is the one it created: an in-place change moves no
        // identity.
        let published = outputs_of(&described)
            .get("StreamName")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            published,
            row.stream(),
            "case {:?}: an in-place destination change keeps the stream's identity, got \
             {published:?} in {described}",
            row.case
        );

        // ...and Firehose itself reports the declared destination, under a NEW version id.
        let described = await_stream_active(&router, row).await;
        assert_destination(row, &described, row.after);
        let after = version_of(row, &described);
        let (parsed_before, parsed_after) = (
            before.parse::<u64>().unwrap_or_default(),
            after.parse::<u64>().unwrap_or_default(),
        );
        assert!(
            parsed_after > parsed_before,
            "case {:?}: expected the applied update to move the version id past {before:?}, got \
             {after:?} in {described}",
            row.case
        );
    }

    // ---- And the data path writes what follows under the configuration now in force. ----
    for row in ROWS {
        put_marker(&router, row, &row.second_marker()).await;
    }
    for row in ROWS {
        let key = await_marker(&router, row, row.after, &row.second_marker()).await;
        assert!(
            key.starts_with(row.after),
            "case {:?}: expected the key of the record put after the update to begin with {:?}, \
             got {key:?}",
            row.case,
            row.after
        );
        // The prefix the destination no longer carries holds exactly what it held before the
        // update: nothing lands there any more.
        let held = keys_under(&router, row, row.before).await;
        assert_eq!(
            held.len(),
            1,
            "case {:?}: expected the superseded prefix {:?} to keep only the object delivered \
             before the update, got {held:?}",
            row.case,
            row.before
        );
    }
}

/// Deploy `row`'s template with `action` (`CreateStack` or `UpdateStack`), with the prefix
/// parameter set to `prefix`.
async fn deploy(router: &MultiServiceRouter, row: &Row, action: &str, prefix: &str) {
    let template = row.template();
    let (status, body) = cloudformation(
        router,
        action,
        &[
            ("StackName", row.stack),
            ("TemplateBody", &template),
            ("Parameters.member.1.ParameterKey", PREFIX_PARAMETER),
            ("Parameters.member.1.ParameterValue", prefix),
            // Every template declares the IAM role its stream delivers under, by name.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from {action}, got {status} with body {body}",
        row.case
    );
}

/// Poll `row`'s stack until it first reports a settled status, and answer the
/// `DescribeStacks` body that status was read from.
async fn await_settled(router: &MultiServiceRouter, row: &Row, expected: &str) -> String {
    let deadline = Instant::now() + DEPLOY_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}",
            row.case
        );
        let reported = element(&body, "StackStatus");
        if reported.ends_with("_COMPLETE") || reported.ends_with("_FAILED") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the stack to settle in {expected}, last saw {reported:?} in \
             {body}",
            row.case
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

/// The `VersionId` a description reports.
fn version_of(row: &Row, description: &Value) -> String {
    description["VersionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the stream to report a VersionId, got {description}",
                row.case
            )
        })
        .to_string()
}

/// Require the one destination Firehose reports for `row`'s stream to be the configuration
/// the template declares, with `prefix` in force.
fn assert_destination(row: &Row, description: &Value, prefix: &str) {
    let destination = &description["Destinations"][0];
    let reported = &destination[row.description];
    assert_eq!(
        reported["Prefix"],
        json!(prefix),
        "case {:?}: expected {} to report the declared Prefix {prefix:?}, got {description}",
        row.case,
        row.description
    );
    if row.error_prefix {
        assert_eq!(
            reported["ErrorOutputPrefix"],
            json!(row.error_output_prefix(prefix)),
            "case {:?}: expected {} to report the declared ErrorOutputPrefix, got {description}",
            row.case,
            row.description
        );
    }
    assert!(
        destination["DestinationId"].as_str().is_some(),
        "case {:?}: expected the destination to carry the id Firehose gave it, got {description}",
        row.case
    );
}

/// Write one record the way `row`'s stream is fed: to the Kinesis data stream it reads, or to
/// the stream itself.
async fn put_marker(router: &MultiServiceRouter, row: &Row, record: &[u8]) {
    let data = ::aws_smithy_types::base64::encode(record);
    let (service, target, request) = match row.source {
        Source::KinesisStream => (
            "kinesis",
            "Kinesis_20131202.PutRecord",
            json!({
                "StreamName": row.source_stream(),
                "PartitionKey": "pk-marker",
                "Data": data,
            }),
        ),
        Source::DirectPut => (
            "firehose",
            "Firehose_20150804.PutRecord",
            json!({
                "DeliveryStreamName": row.stream(),
                "Record": { "Data": data },
            }),
        ),
    };
    let (status, body) = json_rpc(router, service, target, &request).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected the record to be accepted by {target}, got {status} with {body}",
        row.case
    );
}

/// Poll `row`'s bucket until an object under `prefix` holds `record`, and answer its key.
async fn await_marker(
    router: &MultiServiceRouter,
    row: &Row,
    prefix: &str,
    record: &[u8],
) -> String {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let keys = keys_under(router, row, prefix).await;
        for key in &keys {
            if object(router, row, key).await == record {
                return key.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the record {:?} under {prefix:?}, got the keys {keys:?}",
            row.case,
            String::from_utf8_lossy(record),
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every key `row`'s bucket holds under `prefix`, read through Amazon S3's own
/// `ListObjectsV2`. Each row delivers one object per record, so one page holds them all.
async fn keys_under(router: &MultiServiceRouter, row: &Row, prefix: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{}?list-type=2&prefix={prefix}", row.bucket()),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status,
        200,
        "case {:?}: expected HTTP 200 from ListObjectsV2 on {}, got {status} with {text}",
        row.case,
        row.bucket()
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// The bytes one delivered object holds, read through `GetObject`.
async fn object(router: &MultiServiceRouter, row: &Row, key: &str) -> Vec<u8> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{}/{key}", row.bucket()),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {:?}: expected HTTP 200 from GetObject for {key:?}, got {status} with {}",
        row.case,
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
