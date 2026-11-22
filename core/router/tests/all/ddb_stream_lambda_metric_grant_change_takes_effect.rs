// SPDX-License-Identifier: CC0-1.0

//! WHEN a running function's own Amazon CloudWatch publish is authorized: against the
//! identity-based documents the IAM store holds for the function's execution role AT THE INSTANT
//! THE CALL ARRIVES, so a grant written or removed between two records decides the two records
//! differently.
//!
//! RULE: a CloudWatch metric action IAM judges over every resource — publishing a datapoint,
//! which the IAM Service Authorization Reference lists with an empty "Resource types" column —
//! is decided against the identity-based policies in force on the CALLING PRINCIPAL when the
//! request arrives, and the principal of a call an AWS SDK makes from inside a running function
//! is a session of that function's execution role; a call no readable statement allows is
//! refused under the code a client branches on for this class and stores NOTHING, so no reader
//! answers a datapoint for it and the refusal never replays once the grant arrives, while a call
//! a statement does allow stores exactly the datapoints it carried.
//!
//! Each clause is a documented one. "The execution role grants the function permission to use
//! Amazon Web Services services" (`FunctionConfiguration$Role`), and with neither an
//! identity-based nor a resource-based allow a request is denied (IAM User Guide, *Policy
//! evaluation logic*) — a decision about the request being served, so what counts is what the
//! store holds then: "Adds or updates an inline policy document that is embedded in the specified
//! IAM role" (`PutRolePolicy`) and "Deletes the specified inline policy that is embedded in the
//! specified IAM role" (`DeleteRolePolicy`) are both ordinary writes, with no republication step
//! between them and the next request. "Publishes metric data to Amazon CloudWatch. CloudWatch
//! associates the data with the specified metric. If the specified metric does not exist,
//! CloudWatch creates the metric" (`PutMetricData`) is the only way a datapoint of a custom
//! series comes to exist, so a series that was never published into has nothing to answer.
//!
//! Seven crates have to agree for that to hold, and the handler's own SDK call is dispatched back
//! into this same router, so only the ASSEMBLED emulator can show it: CloudFormation archives the
//! inline handler and deploys the roles, IAM holds each role and its inline documents, STS mints
//! the session the handler's calls are signed with, DynamoDB holds the tables and mints the
//! stream, DynamoDB Streams serves its records, Lambda polls that stream and RUNS the handler,
//! and CloudWatch takes the decision and answers the series.
//!
//! What the neighbours pin and this one does not repeat:
//! `all/lambda_in_function_cloudwatch_authorization` drives the DECISION over a table of role
//! documents fixed for the whole run, and `all/ddb_stream_lambda_custom_metric_alarm` drives a
//! stream-triggered handler's custom series and the alarm over it, with each role's document
//! likewise fixed. What is pinned HERE is the one dimension neither covers: the document CHANGING
//! under a live function, in BOTH directions.
//!
//! The rows differ along the direction of that change — a role that starts without the publish
//! grant and gains it, and a role that starts with it and loses it — so neither record's outcome
//! can be a constant, and each row is its own control: the same handler, the same series and the
//! same records, differing only in which document the store held when the publish arrived. Every
//! observation is one a client makes: the marker the handler writes for each record it was handed
//! (which is what tells "the handler ran and published nothing" apart from "the handler never
//! ran") and the series as `GetMetricStatistics` and `GetMetricData` answer it.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, cw, router as emulator,
};
use crate::query_wire::{elements, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The namespace every datapoint and every read below names: a custom one, so nothing here
/// touches a reserved `AWS/` namespace or a series a service publishes by itself.
const NAMESPACE: &str = "Emulator/XsvcGrantChange";

/// The stack the execution roles and the consumer functions are deployed by, and the
/// `StackStatus` it has to reach.
const STACK: &str = "xsvc-grant-change-consumers";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// DynamoDB's `X-Amz-Target` prefix.
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// Where Lambda binds the two collections used below.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// IAM's signing name and the API version its `awsQuery` calls name.
const IAM_SIGNING_NAME: &str = "iam";
const IAM_VERSION: &str = "2010-05-08";

/// The code CloudWatch refuses a call the policies in force do not allow with — what a marker of
/// a refused publish has to carry, so a row's silence is a REFUSAL and not a handler that decided
/// to publish nothing.
const ACCESS_DENIED: &str = "AccessDenied";

/// The period every read below is answered at, in seconds. Every datapoint of one row carries the
/// same time stamp ([`STAMP_VARIABLE`]), so they fall in ONE period of this grid whatever the
/// host's pace, and the read's window reaches two periods either side of that stamp.
const PERIOD: i64 = 300;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the authorization decision and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How many records one mapping takes per batch.
const BATCH_SIZE: u64 = 25;

/// The `StreamViewType` the source tables are created with: the handler reads the item that was
/// written, so the stream has to capture the new image.
const STREAM_VIEW_TYPE: &str = "NEW_IMAGE";

/// Where each mapping starts reading: the oldest record in the shard, so a write made before the
/// mapping's poller first looked is still delivered.
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// The key of the record each phase writes: one handed over while the role carries the document it
/// started with, one handed over after that document changed.
const FIRST_RECORD: &str = "record-before-the-change";
const SECOND_RECORD: &str = "record-after-the-change";

/// How long an observation of work the CLOCK owes — a stack's resources coming up, a function
/// going `Active`, a table reaching `ACTIVE`, a mapping settling, an invocation — waits before
/// calling it a hang, and how long between two looks. Generous: a row fails only when the work did
/// not happen at all, so a loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(120);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The largest difference two figures of the same series may show and still be the same figure:
/// both sides are sums of whole datapoints of one unit, so anything above rounding diverges.
const TOLERANCE: f64 = 1e-9;

/// The environment variables the handler reads its own wiring out of.
const NAMESPACE_VARIABLE: &str = "NAMESPACE";
const METRIC_VARIABLE: &str = "METRIC";
const MARKERS_VARIABLE: &str = "MARKERS";
/// The instant, in epoch seconds, every datapoint the handler publishes is stamped with. Fixing it
/// is what makes a row's publishes land in ONE period however long the host takes over the
/// invocations, so what a period's `Sum` carries is the row's own count.
const STAMP_VARIABLE: &str = "STAMP";

/// The name of the inline document that carries everything a row's role needs whatever phase it is
/// in, and of the one that carries the publish grant alone — separate documents, so the grant can
/// be written and removed without touching the stream reads the poller makes or the marker write
/// the handler makes.
const CONSUME_POLICY: &str = "consume-records";
const PUBLISH_POLICY: &str = "publish-metric";

/// The handler every row deploys inline: for each record it is handed it publishes ONE datapoint of
/// one unit into its own series and records in the marker table what became of that publish —
/// including the code of one CloudWatch refused — so an invocation reports its work whether or not
/// the publish was allowed.
const CONSUMER_HANDLER: &str = r##"
import datetime
import os

import boto3
from botocore.exceptions import ClientError

NAMESPACE = os.environ["NAMESPACE"]
METRIC = os.environ["METRIC"]
MARKERS = os.environ["MARKERS"]
STAMP = datetime.datetime.utcfromtimestamp(float(os.environ["STAMP"]))


def handler(event, context):
    cloudwatch = boto3.client("cloudwatch")
    dynamodb = boto3.client("dynamodb")
    handled = 0
    for record in event["Records"]:
        body = record.get("dynamodb", {})
        image = body.get("NewImage", {})
        key = image.get("pk", {}).get("S", body.get("SequenceNumber", ""))
        marker = {"id": {"S": key}, "published": {"BOOL": False}, "code": {"S": ""}}
        try:
            cloudwatch.put_metric_data(
                Namespace=NAMESPACE,
                MetricData=[
                    {
                        "MetricName": METRIC,
                        "Timestamp": STAMP,
                        "Value": 1.0,
                        "Unit": "Count",
                    }
                ],
            )
            marker["published"] = {"BOOL": True}
        except ClientError as refused:
            marker["code"] = {"S": refused.response["Error"]["Code"]}
        dynamodb.put_item(TableName=MARKERS, Item=marker)
        handled = handled + 1
    return {"handled": handled}
"##;

/// One row of the table below: which way its role's publish grant changes between the two records.
struct Row {
    case: &'static str,
    /// Whether the publish grant is in force when the FIRST record is handed over — and so, since
    /// the change is a flip, not in force when the second one is.
    granted_first: bool,
}

/// The rows: the change in each direction, so neither record's outcome can be a constant and
/// neither row's series can be explained by anything but the document in force at the publish.
const ROWS: &[Row] = &[
    Row {
        case: "a role that gains the publish grant after a record it could not publish for",
        granted_first: false,
    },
    Row {
        case: "a role that loses the publish grant after a record it did publish for",
        granted_first: true,
    },
];

/// The resources of one row, each named off its position so no row can read or write another's.
fn role_name(index: usize) -> String {
    format!("xsvc-grant-change-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-grant-change-consumer-{index}")
}
fn source_table(index: usize) -> String {
    format!("xsvc-grant-change-source-{index}")
}
fn marker_table(index: usize) -> String {
    format!("xsvc-grant-change-markers-{index}")
}
fn metric_name(index: usize) -> String {
    format!("GrantChange{index}")
}

/// Wall-clock now as epoch seconds — the instant a row's datapoints are stamped with and the centre
/// of every read's window.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock is after the epoch")
        .as_secs_f64()
}

/// One DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`) that must answer 200.
async fn ddb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("authorization", authorization("dynamodb", REGION))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{DYNAMODB_TARGET_PREFIX}.{operation}"),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda", REGION))
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One `awsQuery` call against the service whose signing name is `signing_name`, as its status and
/// XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name, REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One IAM call that must answer 200 — the write that changes what a row's role may do.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, members: &[(&str, &str)]) {
    let (status, answered) = query(router, IAM_SIGNING_NAME, IAM_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {answered}"
    );
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The document a row's role carries in every phase: the log actions every execution role needs,
/// the stream reads a DynamoDB-stream mapping's poller makes, and the marker write the handler
/// makes. It never changes, so what changes between the two records is the publish grant alone.
///
/// The actions a DynamoDB-stream poller is decided on are the stream's own three
/// (`DescribeStream`, `GetShardIterator`, `GetRecords`), with the account-wide `ListStreams` beside
/// them as AWS's own execution-role policy for the source writes it.
fn consume_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "logs:CreateLogGroup",
                    "logs:CreateLogStream",
                    "logs:PutLogEvents",
                ],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": [
                    "dynamodb:DescribeStream",
                    "dynamodb:GetShardIterator",
                    "dynamodb:GetRecords",
                    "dynamodb:ListStreams",
                    "dynamodb:PutItem",
                ],
                "Resource": "*",
            },
        ],
    })
}

/// The document that carries the publish grant alone. `PutMetricData` supports no resource types in
/// the IAM Service Authorization Reference, so it is granted over every resource, which is the only
/// way to grant it.
fn publish_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["cloudwatch:PutMetricData"],
            "Resource": "*",
        }],
    })
}

/// The template the suite deploys: one execution role per row, carrying the consume document and —
/// for a row whose first record is published — the publish document beside it, and one consumer
/// function per row running as that role. The inline source is archived into the deployment package
/// by CloudFormation, which is how a function whose handler is written here comes to exist at all.
fn stack_template(stamp: f64) -> String {
    let stamp = format!("{stamp:.0}");
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_logical = format!("Role{index}");
        let mut policies = vec![json!({
            "PolicyName": CONSUME_POLICY,
            "PolicyDocument": consume_document(),
        })];
        if row.granted_first {
            policies.push(json!({
                "PolicyName": PUBLISH_POLICY,
                "PolicyDocument": publish_document(),
            }));
        }
        resources.insert(
            role_logical.clone(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role_name(index),
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": policies,
                },
            }),
        );
        resources.insert(
            format!("Consumer{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {
                        NAMESPACE_VARIABLE: NAMESPACE,
                        METRIC_VARIABLE: metric_name(index),
                        MARKERS_VARIABLE: marker_table(index),
                        STAMP_VARIABLE: stamp.clone(),
                    }},
                    "Code": {"ZipFile": CONSUMER_HANDLER},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the stack and wait for `CREATE_COMPLETE`, reporting what failed when it does not reach it.
async fn deploy(router: &MultiServiceRouter, stamp: f64) {
    let template = stack_template(stamp);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares named IAM roles, which a create acknowledges before
            // CloudFormation provisions them.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let (reported, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {name:?} Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `CreateTable` body of a row's SOURCE table: a stream-enabled table whose stream captures the
/// image the handler reads.
fn source_table_body(name: &str) -> Value {
    json!({
        "TableName": name,
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "BillingMode": "PAY_PER_REQUEST",
        "StreamSpecification": {"StreamEnabled": true, "StreamViewType": STREAM_VIEW_TYPE},
    })
}

/// The `CreateTable` body of a row's MARKER table, keyed by the record the marker is about — so a
/// record handed over twice leaves one marker, not two.
fn marker_table_body(name: &str) -> Value {
    json!({
        "TableName": name,
        "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
        "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}],
        "BillingMode": "PAY_PER_REQUEST",
    })
}

/// Poll `DescribeTable` until `name` reports `ACTIVE`, which is when it serves.
async fn wait_until_table_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ddb(router, case, "DescribeTable", &json!({"TableName": name})).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table {name:?} never reached ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The enabled mapping over `stream_arn`, polled until Lambda reports it `Enabled`.
async fn given_mapping(router: &MultiServiceRouter, case: &str, function: &str, stream_arn: &str) {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        Some(&json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": STARTING_POSITION,
            "BatchSize": BATCH_SIZE,
            "Enabled": true,
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {status} with \
         {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the created mapping reports no UUID, got {created}")
        })
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        if status == 200 && read["State"].as_str() == Some("Enabled") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} never reported Enabled, last read {status} {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One record into a row's source table, which is what hands the handler an invocation.
async fn write_record(router: &MultiServiceRouter, case: &str, table: &str, key: &str) {
    ddb(
        router,
        case,
        "PutItem",
        &json!({"TableName": table, "Item": {"pk": {"S": key}}}),
    )
    .await;
}

/// The marker the handler recorded for `key`, looked at again until it exists or the deadline
/// passes — the wait is on the CONDITION that the handler reported on that record, never on a fixed
/// span. The handler writes the marker AFTER its publish has been answered, so a marker that exists
/// is an answer the series has already seen whatever it was going to see.
async fn awaited_marker(router: &MultiServiceRouter, case: &str, table: &str, key: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = ddb(
            router,
            case,
            "GetItem",
            &json!({
                "TableName": table,
                "Key": {"id": {"S": key}},
                "ConsistentRead": true,
            }),
        )
        .await;
        if read["Item"].is_object() {
            return read["Item"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the handler never reported on the record {key:?}, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What `GetMetricStatistics` answers for `metric` over the window around `stamp`: how many
/// datapoints it reported and the total of their `Sum`.
async fn statistics(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    stamp: f64,
) -> (usize, f64) {
    let answered = cw(
        router,
        case,
        REGION,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "StartTime": stamp - 2.0 * PERIOD as f64,
            "EndTime": stamp + 2.0 * PERIOD as f64,
            "Period": PERIOD,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    let points = answered["Datapoints"].as_array().cloned().unwrap_or_default();
    let total = points.iter().filter_map(|point| point["Sum"].as_f64()).sum();
    (points.len(), total)
}

/// The same series through the OTHER reader: what `GetMetricData` answers for `metric` over the
/// same window — how many values it reported and their total.
async fn metric_data(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    stamp: f64,
) -> (usize, f64) {
    let answered = cw(
        router,
        case,
        REGION,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{
                "Id": "published",
                "MetricStat": {
                    "Metric": {"Namespace": NAMESPACE, "MetricName": metric},
                    "Period": PERIOD,
                    "Stat": "Sum",
                },
                "ReturnData": true,
            }],
            "StartTime": stamp - 2.0 * PERIOD as f64,
            "EndTime": stamp + 2.0 * PERIOD as f64,
        }),
    )
    .await;
    let values = answered["MetricDataResults"][0]["Values"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total = values.iter().filter_map(Value::as_f64).sum();
    (values.len(), total)
}

/// What a row's marker has to say about one record: whether the publish was allowed, and — when it
/// was not — under which code it was refused.
fn assert_marker(case: &str, phase: &str, marker: &Value, published: bool) {
    assert_eq!(
        marker["published"]["BOOL"],
        json!(published),
        "case {case:?}: expected the {phase} record's publish to be {}, got {marker}",
        if published { "allowed" } else { "refused" }
    );
    let code = marker["code"]["S"].as_str().unwrap_or_default();
    if published {
        assert!(
            code.is_empty(),
            "case {case:?}: an allowed publish reports no refusal, got {code:?} in {marker}"
        );
    } else {
        assert_eq!(
            code, ACCESS_DENIED,
            "case {case:?}: a withheld publish must be refused under {ACCESS_DENIED}, got \
             {code:?} in {marker}"
        );
    }
}

/// What both readers say the series holds, which must be the same figure through either of them.
async fn assert_series(
    router: &MultiServiceRouter,
    case: &str,
    phase: &str,
    metric: &str,
    stamp: f64,
    expected: f64,
) {
    let (statistics_points, statistics_sum) = statistics(router, case, metric, stamp).await;
    let (data_points, data_sum) = metric_data(router, case, metric, stamp).await;
    assert_eq!(
        statistics_points > 0,
        expected > 0.0,
        "case {case:?}: {phase}, expected the {metric:?} series to hold datapoints only where a \
         publish was allowed (a Sum of {expected}), GetMetricStatistics reported \
         {statistics_points} datapoint(s) totalling {statistics_sum}"
    );
    assert_eq!(
        data_points > 0,
        expected > 0.0,
        "case {case:?}: {phase}, expected the {metric:?} series to hold values only where a \
         publish was allowed (a Sum of {expected}), GetMetricData reported {data_points} value(s) \
         totalling {data_sum}"
    );
    assert!(
        (statistics_sum - expected).abs() < TOLERANCE,
        "case {case:?}: {phase}, expected GetMetricStatistics to report a Sum of {expected} for \
         {metric:?}, got {statistics_sum} over {statistics_points} datapoint(s)"
    );
    assert!(
        (data_sum - expected).abs() < TOLERANCE,
        "case {case:?}: {phase}, expected GetMetricData to report {expected} for {metric:?}, got \
         {data_sum} over {data_points} value(s)"
    );
}

/// RULE: a running function's publish is decided against the identity-based documents the IAM store
/// holds for its execution role when the call arrives — so the same handler, publishing the same
/// datapoint into the same series, is refused for the record handed over while no readable
/// statement allows `cloudwatch:PutMetricData` and served for the record handed over once one does,
/// in either direction of the change; a refused publish stores nothing, which no reader answers and
/// no later grant replays.
#[tokio::test]
async fn an_execution_roles_publish_grant_decides_the_record_that_arrives_after_it() {
    let router = emulator();
    // One instant every datapoint of this run is stamped with, so each row's publishes land in one
    // period of the readers' grid however long the host takes over the invocations.
    let stamp = now_secs();

    // 1. The execution roles and the consumer functions, deployed together.
    deploy(&router, stamp).await;
    for (index, row) in ROWS.iter().enumerate() {
        wait_until_active(&router, row.case, &function_name(index)).await;
    }

    // 2. Each row's stream-enabled source table and its marker table. Every create is made before
    //    any wait, so the spans those creates owe overlap.
    let mut stream_arns: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let created = ddb(
            &router,
            row.case,
            "CreateTable",
            &source_table_body(&source_table(index)),
        )
        .await;
        let arn = created["TableDescription"]["LatestStreamArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the create must report the stream it enabled, got {created}",
                    row.case
                )
            })
            .to_string();
        stream_arns.push(arn);
        ddb(
            &router,
            row.case,
            "CreateTable",
            &marker_table_body(&marker_table(index)),
        )
        .await;
    }
    for (index, row) in ROWS.iter().enumerate() {
        wait_until_table_active(&router, row.case, &source_table(index)).await;
        wait_until_table_active(&router, row.case, &marker_table(index)).await;
    }

    // 3. The mappings onto the streams DynamoDB minted.
    for (index, row) in ROWS.iter().enumerate() {
        given_mapping(&router, row.case, &function_name(index), &stream_arns[index]).await;
    }

    // 4. The first record of each row, handed over while the role still carries the document it was
    //    deployed with.
    for (index, row) in ROWS.iter().enumerate() {
        write_record(&router, row.case, &source_table(index), FIRST_RECORD).await;
    }
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = awaited_marker(&router, case, &marker_table(index), FIRST_RECORD).await;
        assert_marker(case, "first", &marker, row.granted_first);
        let expected = if row.granted_first { 1.0 } else { 0.0 };
        assert_series(
            &router,
            case,
            "before the change",
            &metric_name(index),
            stamp,
            expected,
        )
        .await;
    }

    // 5. The change itself, made with IAM's own writes on the SAME role the function runs as: the
    //    row that started without the grant is given it, and the row that started with it has it
    //    removed. The document the poller and the marker write need is left alone either way.
    for (index, row) in ROWS.iter().enumerate() {
        let role = role_name(index);
        if row.granted_first {
            iam(
                &router,
                row.case,
                "DeleteRolePolicy",
                &[("RoleName", &role), ("PolicyName", PUBLISH_POLICY)],
            )
            .await;
        } else {
            iam(
                &router,
                row.case,
                "PutRolePolicy",
                &[
                    ("RoleName", &role),
                    ("PolicyName", PUBLISH_POLICY),
                    ("PolicyDocument", &publish_document().to_string()),
                ],
            )
            .await;
        }
    }

    // 6. The second record of each row, handed over under the changed document.
    for (index, row) in ROWS.iter().enumerate() {
        write_record(&router, row.case, &source_table(index), SECOND_RECORD).await;
    }
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = awaited_marker(&router, case, &marker_table(index), SECOND_RECORD).await;
        assert_marker(case, "second", &marker, !row.granted_first);
        // Exactly ONE of the two records was published whichever way the change went, so the series
        // carries one datapoint of one unit: the refused publish neither landed when it was made
        // nor replayed when the grant arrived.
        assert_series(
            &router,
            case,
            "after the change",
            &metric_name(index),
            stamp,
            1.0,
        )
        .await;
    }
}
