// SPDX-License-Identifier: CC0-1.0

//! What a CUSTOM metric a stream-processing function publishes about its own work is worth to
//! the client that instrumented it: the whole path from a table write to an alarm's state,
//! across the services that own the halves of it — DynamoDB holds the table and mints its
//! stream, DynamoDB Streams serves that stream's shards and records, IAM holds the execution
//! role the poller reads as and the handler's own calls are judged against, STS mints the
//! session those calls are signed with, CloudFormation archives the inline handler into the
//! function's deployment package, Lambda polls the stream and RUNS the handler, and CloudWatch
//! takes the publish, answers the series and evaluates the alarm over it.
//!
//! The RULE pinned here: **a datapoint a RUNNING function publishes with `PutMetricData` is
//! stored in the series the call names and is read back by both metric readers — the `Sum` over
//! a period holding N such datapoints of one unit each is N, so a handler publishing once per
//! record it acted on reports exactly the number of records it acted on and not the number it
//! was handed — and a metric alarm over that series reports the state its OWN comparison of
//! that statistic against its OWN threshold computes (`ALARM` once the comparison holds, `OK`
//! while it does not), with no call forcing the transition; a publish the policies in force on
//! the function's execution role do not allow stores nothing, so the series has no datapoint at
//! all and the alarm stays in the `INSUFFICIENT_DATA` its creation left it in.**
//!
//! Each clause is a documented one. "Publishes metric data to Amazon CloudWatch. CloudWatch
//! associates the data with the specified metric. If the specified metric does not exist,
//! CloudWatch creates the metric" (`PutMetricData`); an alarm's state is "The arithmetic
//! operation to use when comparing the specified statistic and threshold"
//! (`MetricAlarm$ComparisonOperator`) applied to the statistic over its evaluation periods, so
//! it is a value CloudWatch COMPUTES and not one a caller writes; "When this operation creates
//! an alarm, the alarm state is immediately set to INSUFFICIENT_DATA. The alarm is then
//! evaluated and its state is set appropriately" (`PutMetricAlarm`), and with the default
//! `TreatMissingData` of `missing` an evaluation that found no datapoint decides nothing, which
//! leaves the alarm there; and the execution role is what an in-function call is judged against
//! ("The execution role grants the function permission to use Amazon Web Services services",
//! `FunctionConfiguration$Role`), so a role short of the publish's action publishes nothing.
//!
//! Seven crates have to agree for that to hold, and the handler's own SDK call is dispatched
//! back into this same router, so only the ASSEMBLED emulator can show it: a crate-local suite
//! sees one side. What the neighbours pin and this one does not repeat:
//! `all/ddb_stream_lambda_esm_provisioning` pins that a stream-enabled table's stream is
//! mappable at all, `all/lambda_in_function_cloudwatch_authorization` pins the DECISION a
//! running function's metric call is subject to over a table of role documents,
//! `all/lambda_sqs_event_source_failed_batch_keeps_its_message` pins what a mapping hands a
//! function and what it does with a batch that failed, and
//! `all/dynamodb_write_capacity_metrics_drive_the_alarm_chain` pins the alarm chain over a
//! series the SERVICE publishes. What is pinned HERE is the client's own instrumentation: a
//! CUSTOM series a handler fills one datapoint at a time, and the alarm that series decides.
//!
//! Every observation is one a client makes: the marker table the handler records each record it
//! was handed in — which is what tells "the handler ran and published nothing" apart from "the
//! handler never ran" — the series as `GetMetricStatistics` and `GetMetricData` answer it, and
//! the alarm's own `StateValue` from `DescribeAlarms`.
//!
//! The rows differ along what the rule generalises over: how many of the inserts the handler
//! skips and how many it keeps (so the figure is the count of the ones it acted on, not of the
//! batch), where that count falls against the threshold (at it, past it), WHICH comparison the
//! alarm is configured with (the strict one and the inclusive one, which decide the boundary
//! differently), and whether the execution role allows the publish at all. Two rows publish the
//! SAME count under different thresholds and end in different states, so no row's state can be
//! a constant; and the row with no grant has a GRANTED TWIN on the same counts and the same
//! alarm configuration, differing only in the grant, so its silence is read against a row that
//! did move rather than against the state its own creation left behind.

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
const NAMESPACE: &str = "Emulator/XsvcSkippedRecords";

/// The stack the execution roles and the consumer functions are deployed by, and the
/// `StackStatus` it has to reach.
const STACK: &str = "xsvc-skipped-records-consumers";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// DynamoDB's `X-Amz-Target` prefix.
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// Where Lambda binds the two collections used below.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The code CloudWatch refuses a call the policies in force do not allow with — what a marker
/// of a refused publish has to carry, so a row's silence is a REFUSAL and not a handler that
/// decided to publish nothing.
const ACCESS_DENIED: &str = "AccessDenied";

/// The period every alarm below is configured with and every read below is answered at, in
/// seconds. Each row's datapoints all carry the same time stamp ([`STAMP_VARIABLE`]), so they
/// fall in ONE period of this grid whatever the host's pace, and the read's window reaches two
/// periods either side of that stamp.
const PERIOD: i64 = 300;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the metric path and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How many records one mapping takes per batch: above every row's record count, so a row's
/// whole write burst can be handed over in one invocation.
const BATCH_SIZE: u64 = 25;

/// The `StreamViewType` the source tables are created with: the handler reads the item that was
/// written, so the stream has to capture the new image.
const STREAM_VIEW_TYPE: &str = "NEW_IMAGE";

/// Where each mapping starts reading: the oldest record in the shard, so a write made before
/// the mapping's poller first looked is still delivered.
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// The attribute every source item carries the handler's verdict in, and the two values it
/// takes: one the handler acts on (and publishes a datapoint for) and one it does not.
const VERDICT_ATTRIBUTE: &str = "verdict";
const SKIP: &str = "skip";
const KEEP: &str = "keep";

/// The three modelled `StateValue` values an alarm reports
/// (`smithy_query(query="shape",shape="StateValue")`: `ALARM | INSUFFICIENT_DATA | OK`).
const STATE_OK: &str = "OK";
const STATE_ALARM: &str = "ALARM";
const STATE_INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// How long an observation of work the CLOCK owes — a function going `Active`, a table reaching
/// `ACTIVE`, a mapping settling, an invocation, an alarm's evaluation — waits before calling it
/// a hang, and how long between two looks. Generous: a row fails only when the work did not
/// happen at all, so a loaded host cannot decide the outcome.
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
/// The instant, in epoch seconds, every datapoint the handler publishes is stamped with. Fixing
/// it is what makes the row's datapoints land in ONE period however long the host takes to run
/// the invocations, so the figure a period's `Sum` carries is the row's own count and not a
/// burst split across a period boundary.
const STAMP_VARIABLE: &str = "STAMP";

/// The handler every row deploys inline: for each record it is handed it reads the verdict off
/// the new image, publishes ONE datapoint of one unit for a record it acted on, records in the
/// marker table what it did with that record — including the code of a publish CloudWatch
/// refused — and goes on to the next, so an invocation reports its work whether or not the
/// publish was allowed.
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
        verdict = image.get("verdict", {}).get("S", "")
        marker = {
            "id": {"S": body.get("SequenceNumber", "")},
            "verdict": {"S": verdict},
            "published": {"BOOL": False},
        }
        if verdict == "skip":
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

/// One row of the table below.
struct Row {
    case: &'static str,
    /// How many inserts the handler ACTS on, and so how many datapoints it publishes.
    skipped: usize,
    /// How many inserts it is handed and does NOT act on, which must leave no datapoint.
    kept: usize,
    /// The alarm's own threshold and comparison.
    threshold: f64,
    comparison: &'static str,
    /// Whether the execution role allows the publish at all.
    granted: bool,
    /// The `StateValue` the alarm has to end up reporting.
    state: &'static str,
}

/// The rows: the count against the threshold (at it, past it), under each of the two
/// comparisons that decide the boundary differently, and the same work with the publish
/// withheld.
///
/// `GreaterThanThreshold` and `GreaterThanOrEqualToThreshold` are two of the comparisons
/// `MetricAlarm$ComparisonOperator` declares
/// (`smithy_query(query="shape",shape="ComparisonOperator")`); the whole catalog of them is
/// tabled in the owning crate's own alarm-evaluation suites, so the rows here span the one
/// dimension this suite adds — a boundary decided over a series a HANDLER filled.
const ROWS: &[Row] = &[
    Row {
        case: "as many skips as the threshold, with kept inserts beside them, holds OK",
        skipped: 3,
        kept: 2,
        threshold: 3.0,
        comparison: "GreaterThanThreshold",
        granted: true,
        state: STATE_OK,
    },
    Row {
        case: "the same skips one past a lower threshold fire the alarm",
        skipped: 3,
        kept: 1,
        threshold: 2.0,
        comparison: "GreaterThanThreshold",
        granted: true,
        state: STATE_ALARM,
    },
    Row {
        case: "at the threshold under the inclusive comparison fires the alarm",
        skipped: 2,
        kept: 0,
        threshold: 2.0,
        comparison: "GreaterThanOrEqualToThreshold",
        granted: true,
        state: STATE_ALARM,
    },
    Row {
        case: "a role that may not publish leaves the series empty and the alarm untouched",
        // The granted TWIN of this row is the second one above: the same counts, threshold and
        // comparison, differing only in the grant, which is what makes this row's silence a
        // decision rather than the state its own creation left behind.
        skipped: 3,
        kept: 1,
        threshold: 2.0,
        comparison: "GreaterThanThreshold",
        granted: false,
        state: STATE_INSUFFICIENT_DATA,
    },
];

/// The resources of one row, each named off its position so no row can read or write another's.
fn role_name(index: usize) -> String {
    format!("xsvc-skipped-records-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-skipped-records-consumer-{index}")
}
fn source_table(index: usize) -> String {
    format!("xsvc-skipped-source-{index}")
}
fn marker_table(index: usize) -> String {
    format!("xsvc-skipped-markers-{index}")
}
fn metric_name(index: usize) -> String {
    format!("SkippedRecords{index}")
}
fn alarm_name(index: usize) -> String {
    format!("xsvc-skipped-records-alarm-{index}")
}

/// Wall-clock now as epoch seconds — the instant the row's datapoints are stamped with and the
/// centre of every read's window.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
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

/// One CloudFormation `awsQuery` call, as its status and XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("authorization", authorization("cloudformation", REGION))
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

/// The inline document one row's execution role carries: the log actions every execution role
/// needs, the stream reads a DynamoDB-stream mapping's poller makes, the marker write the
/// handler makes — and, for a granted row, the publish itself.
///
/// The actions a DynamoDB-stream poller is decided on are the stream's own three
/// (`DescribeStream`, `GetShardIterator`, `GetRecords`), with the account-wide `ListStreams`
/// beside them as AWS's own execution-role policy for the source writes it. `PutMetricData`
/// supports no resource types in the IAM Service Authorization Reference, so it is granted over
/// every resource, which is the only way to grant it.
fn role_document(granted: bool) -> Value {
    let mut statements = vec![
        json!({
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogGroup",
                "logs:CreateLogStream",
                "logs:PutLogEvents",
            ],
            "Resource": "*",
        }),
        json!({
            "Effect": "Allow",
            "Action": [
                "dynamodb:DescribeStream",
                "dynamodb:GetShardIterator",
                "dynamodb:GetRecords",
                "dynamodb:ListStreams",
                "dynamodb:PutItem",
            ],
            "Resource": "*",
        }),
    ];
    if granted {
        statements.push(json!({
            "Effect": "Allow",
            "Action": ["cloudwatch:PutMetricData"],
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The template the suite deploys: one execution role per row carrying that row's document, and
/// one consumer function per row running as that role. The inline source is archived into the
/// deployment package by CloudFormation, which is how a function whose handler is written here
/// comes to exist at all.
fn stack_template(stamp: f64) -> String {
    let stamp = format!("{:.0}", stamp);
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_logical = format!("Role{index}");
        resources.insert(
            role_logical.clone(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role_name(index),
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "consume-and-publish",
                        "PolicyDocument": role_document(row.granted),
                    }],
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

/// Deploy the stack and wait for `CREATE_COMPLETE`, reporting what failed when it does not
/// reach it.
async fn deploy(router: &MultiServiceRouter, stamp: f64) {
    let template = stack_template(stamp);
    let (status, answered) = cloudformation(
        router,
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
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
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
        let (_, configuration) =
            lambda(router, "GET", &format!("{FUNCTIONS_URI}/{name}/configuration"), None).await;
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

/// The `CreateTable` body of a row's SOURCE table: a stream-enabled table whose stream captures
/// the image the handler reads.
fn source_table_body(name: &str) -> Value {
    json!({
        "TableName": name,
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "BillingMode": "PAY_PER_REQUEST",
        "StreamSpecification": {"StreamEnabled": true, "StreamViewType": STREAM_VIEW_TYPE},
    })
}

/// The `CreateTable` body of a row's MARKER table, keyed by the sequence number of the record
/// the marker is about — so a record handed over twice leaves one marker, not two.
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

/// The alarm over a row's series, created BEFORE any datapoint exists so every state below is
/// one the publishes drove. `TreatMissingData` is left unset, which is the documented default
/// (`missing`).
async fn given_alarm(router: &MultiServiceRouter, row: &Row, name: &str, metric: &str) {
    cw(
        router,
        row.case,
        REGION,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": metric,
            // One datapoint per record the handler acted on, so what the period is worth is
            // their SUM.
            "Statistic": "Sum",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": row.threshold,
            "ComparisonOperator": row.comparison,
        }),
    )
    .await;
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
        .unwrap_or_else(|| panic!("case {case:?}: the created mapping reports no UUID, got {created}"))
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

/// Every marker the handler has recorded in `table`, looked at again until it holds `wanted` of
/// them or the deadline passes — the wait is on the CONDITION that the handler reported on every
/// record, never on a fixed span.
async fn awaited_markers(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let scanned = ddb(router, case, "Scan", &json!({"TableName": table})).await;
        let held = scanned["Items"].as_array().cloned().unwrap_or_default();
        if held.len() >= wanted {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to report on {wanted} record(s), it reported on \
             {} ({held:?})",
            held.len()
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
    let total = points
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum();
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
                "Id": "skipped",
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

/// What `DescribeAlarms` reports for `name`, looked at again until its `StateValue` is `wanted`
/// or the deadline passes. Answers the whole alarm as it was last read, so an assertion can name
/// both the state and the series it was computed over.
async fn awaited_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = cw(
            router,
            case,
            REGION,
            "DescribeAlarms",
            &json!({"AlarmNames": [name]}),
        )
        .await;
        let alarm = described["MetricAlarms"][0].clone();
        if alarm["StateValue"].as_str() == Some(wanted) {
            return alarm;
        }
        if Instant::now() >= deadline {
            return alarm;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a datapoint a running function publishes is stored and read back by both metric
/// readers, so the `Sum` of a period is the number of records the handler acted on; a metric
/// alarm over that series reports what its own comparison against its own threshold computes,
/// with no call forcing it; and a publish the execution role does not allow stores nothing, so
/// the series stays empty and the alarm stays in the `INSUFFICIENT_DATA` its creation left it
/// in.
#[tokio::test]
async fn a_handlers_custom_metric_sums_its_skips_and_decides_its_alarm() {
    let router = emulator();
    // One instant every datapoint of this run is stamped with, so each row's publishes land in
    // one period of the alarm's grid however long the host takes over the invocations.
    let stamp = now_secs();

    // 1. The execution roles and the consumer functions, deployed together.
    deploy(&router, stamp).await;
    for (index, row) in ROWS.iter().enumerate() {
        wait_until_active(&router, row.case, &function_name(index)).await;
    }

    // 2. Each row's stream-enabled source table and its marker table. Every create is made
    //    before any wait, so the spans those creates owe overlap.
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

    // 3. The alarms, created while every series is still empty.
    for (index, row) in ROWS.iter().enumerate() {
        given_alarm(&router, row, &alarm_name(index), &metric_name(index)).await;
    }

    // 4. The mappings onto the streams DynamoDB minted.
    for (index, row) in ROWS.iter().enumerate() {
        given_mapping(
            &router,
            row.case,
            &function_name(index),
            &stream_arns[index],
        )
        .await;
    }

    // 5. The writes: the records the handler acts on, and the ones it does not.
    for (index, row) in ROWS.iter().enumerate() {
        let table = source_table(index);
        for (position, verdict) in std::iter::repeat_n(SKIP, row.skipped)
            .chain(std::iter::repeat_n(KEEP, row.kept))
            .enumerate()
        {
            ddb(
                &router,
                row.case,
                "PutItem",
                &json!({
                    "TableName": table,
                    "Item": {
                        "pk": {"S": format!("item-{position}")},
                        VERDICT_ATTRIBUTE: {"S": verdict},
                    },
                }),
            )
            .await;
        }
    }

    // 6. What each row's instrumentation is worth to a client, and the state each row's alarm
    //    ends up in.
    let mut reported: Vec<String> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let metric = metric_name(index);
        let delivered = row.skipped + row.kept;

        // The handler reported on every record it was handed — which is what tells a row that
        // published nothing apart from a row whose handler never ran.
        let markers = awaited_markers(&router, case, &marker_table(index), delivered).await;
        assert_eq!(
            markers.len(),
            delivered,
            "case {case:?}: expected one marker per record handed over ({delivered}), got \
             {markers:?}"
        );
        let expected_publishes = if row.granted { row.skipped } else { 0 };
        let published = markers
            .iter()
            .filter(|marker| marker["published"]["BOOL"] == json!(true))
            .count();
        assert_eq!(
            published, expected_publishes,
            "case {case:?}: expected {expected_publishes} published datapoint(s) reported by the \
             handler, got {published} in {markers:?}"
        );
        for marker in &markers {
            let verdict = marker[VERDICT_ATTRIBUTE]["S"].as_str().unwrap_or_default();
            let acted_on = verdict == SKIP;
            let reported = marker["published"]["BOOL"] == json!(true);
            assert_eq!(
                reported,
                acted_on && row.granted,
                "case {case:?}: a record whose verdict is {verdict:?} must be published only \
                 when the handler acts on it and may publish, got {marker}"
            );
            if acted_on && !row.granted {
                assert_eq!(
                    marker["code"]["S"].as_str(),
                    Some(ACCESS_DENIED),
                    "case {case:?}: a withheld publish must be refused under {ACCESS_DENIED}, \
                     got {marker}"
                );
            }
        }

        // The series: one datapoint per record acted on, summed over the period they landed in,
        // and the same figure through either reader.
        let expected_sum = expected_publishes as f64;
        let (statistics_points, statistics_sum) =
            statistics(&router, case, &metric, stamp).await;
        let (data_points, data_sum) = metric_data(&router, case, &metric, stamp).await;
        assert_eq!(
            statistics_points > 0,
            expected_publishes > 0,
            "case {case:?}: expected the {metric:?} series to hold datapoints only where the \
             publish was allowed, GetMetricStatistics reported {statistics_points}"
        );
        assert_eq!(
            data_points > 0,
            expected_publishes > 0,
            "case {case:?}: expected the {metric:?} series to hold values only where the publish \
             was allowed, GetMetricData reported {data_points}"
        );
        assert!(
            (statistics_sum - expected_sum).abs() < TOLERANCE,
            "case {case:?}: expected GetMetricStatistics to report a Sum of {expected_sum} for \
             {metric:?}, got {statistics_sum}"
        );
        assert!(
            (data_sum - expected_sum).abs() < TOLERANCE,
            "case {case:?}: expected GetMetricData to report {expected_sum} for {metric:?}, got \
             {data_sum}"
        );

        // And the alarm over it, moved by nothing but the publishes.
        let alarm = awaited_alarm(&router, case, &alarm_name(index), row.state).await;
        assert_eq!(
            alarm["StateValue"].as_str(),
            Some(row.state),
            "case {case:?}: expected the alarm over {metric:?} ({} {}) to report {:?} for a Sum \
             of {expected_sum}, got {alarm}",
            row.comparison,
            row.threshold,
            row.state
        );
        assert_eq!(
            alarm["MetricName"].as_str(),
            Some(metric.as_str()),
            "case {case:?}: the alarm must report the series it watches, got {alarm}"
        );
        reported.push(alarm["StateValue"].as_str().unwrap_or_default().to_string());
    }

    // The state a row with no grant reports is the one `PutMetricAlarm` left behind, so on its
    // own it says nothing about whether anything was evaluated. Its GRANTED TWIN — the row with
    // the same record counts, threshold and comparison, differing ONLY in the grant — is what
    // makes it a decision: the same work under the same alarm configuration ends in a different
    // state, which no constant and no un-evaluated alarm can produce.
    for (index, row) in ROWS.iter().enumerate() {
        if row.granted {
            continue;
        }
        let twin = ROWS.iter().enumerate().find(|(other, candidate)| {
            *other != index
                && candidate.granted
                && candidate.skipped == row.skipped
                && candidate.kept == row.kept
                && candidate.threshold == row.threshold
                && candidate.comparison == row.comparison
        });
        let (twin_index, twin_row) = twin.unwrap_or_else(|| {
            panic!(
                "case {:?}: this row needs a granted twin on the same counts and alarm \
                 configuration for its silence to mean anything",
                row.case
            )
        });
        assert_ne!(
            reported[index], reported[twin_index],
            "case {:?}: the same {} skip(s) under the same alarm ({} {}) must end in a different \
             state without the publish grant than with it ({:?}), got {:?}",
            row.case,
            row.skipped,
            row.comparison,
            row.threshold,
            twin_row.case,
            reported[index]
        );
    }
}
