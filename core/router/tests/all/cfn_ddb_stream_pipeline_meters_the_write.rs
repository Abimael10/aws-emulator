// SPDX-License-Identifier: CC0-1.0

//! What the `AWS/Lambda` `IteratorAge` of a stream-processing estate DEPLOYED FROM A TEMPLATE is
//! worth to the client reading its dashboard: the lag a widget over that series shows has to be a
//! lag the stream really had.
//!
//! The RULE pinned here: **the age Lambda publishes for a batch is the span between the WRITE of
//! that batch's newest record and the read that took the batch off the shard** — "Age is the
//! difference between the time Lambda received the batch and the time the last record in the batch
//! was written to the stream" (the AWS Lambda metric reference for `IteratorAge`, namespace
//! `AWS/Lambda`, unit `Milliseconds`, dimension `FunctionName`) — so no datapoint of the series is
//! larger than the time that has really elapsed since that write, whatever sub-second phase the
//! write fell on, and none is smaller than the wait the record already had before any poller could
//! read it.
//!
//! What makes this a rule rather than a restatement of the implementation: the stamp a CONSUMER of
//! a table's stream reads for a record, `StreamRecord.ApproximateCreationDateTime`, is documented
//! as "in UNIX epoch time format and rounded down to the closest second". It therefore names a
//! moment up to a second BEFORE the write, and an age measured from it is up to a second longer
//! than the record ever waited — a backlog the client's dashboard shows and the stream never had.
//! A client that alarms on the lag, or asserts a caught-up consumer reads near zero, can only do so
//! against an age bounded by the real elapsed time.
//!
//! What only the assembled emulator can ask, and what this suite adds to its neighbours: here the
//! table, the function, the mapping and the dashboard are all DECLARED IN ONE TEMPLATE, so the
//! stream the mapping meters is the one CloudFormation published to it from the table's own
//! `StreamArn` attribute — six crates have to agree (CloudFormation deploys, IAM holds the role,
//! DynamoDB owns the table and mints the stream, DynamoDB Streams holds the records and the instant
//! of each write, Lambda polls the shard and meters each batch, CloudWatch answers the series).
//! `all/cfn_ddb_stream_lambda_pipeline` pins that such a template's dashboard and tables are
//! created at all, `all/ddb_stream_mapping_meters_the_records_own_wait` pins the same age rule over
//! a mapping a client creates directly, and the Lambda crate's own
//! `stream_mapping_iterator_age_series` pins WHICH series the age is published as; none of them
//! reads the series of a mapping a template wired up.
//!
//! The rows differ along what the rule generalises over: the sub-second phase the batch's writes
//! fell on — which is exactly what rounding a stamp down to the second discards — how many records
//! the batch holds, and one row whose item is written through DynamoDB's own `PutItem`, taking the
//! live wall clock's own phase, which nobody chooses.
//!
//! Nothing is asserted here about the INVOCATION a batch is then handed to: the age is a property of
//! the read, published before anything is handed over, and the function's deployment package is
//! opaque.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams_crate;
use smithy_server_runtime::MultiServiceRouter;
use streams_crate::ops::types::AttributeValue;
use streams_crate::state::{
    floor_to_second, AttributeMap,
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, OperationType,
    RecordAppend,
};

use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::dynamodb;
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::cw;
use crate::query_wire::{elements, outputs_of};

/// The `StackStatus` a completed create reports, from the model's own enum.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack the whole estate is deployed as.
const STACK: &str = "metered-stream-pipeline";

/// The logical id and the name the one execution role every declared function reports is declared
/// under.
const ROLE: &str = "ConsumerRole";
const ROLE_NAME: &str = "metered-pipeline-consumer-role";

/// The logical id and the name of the dashboard the estate declares over the series under test.
const DASHBOARD: &str = "PipelineDashboard";
const DASHBOARD_NAME: &str = "metered-pipeline-dashboard";

/// The partition key every declared table takes, and the `StreamViewType` it is declared with.
const KEY_ATTRIBUTE: &str = "pk";
const STREAM_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";

/// Where each declared mapping starts reading: the oldest record of the shard, so a record already
/// in the stream when the poller first looks is still delivered.
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// The series the age is read back through, as the AWS Lambda metric reference names it.
const NAMESPACE: &str = "AWS/Lambda";
const ITERATOR_AGE: &str = "IteratorAge";
const MILLISECONDS: &str = "Milliseconds";
const FUNCTION_DIMENSION: &str = "FunctionName";

/// The `Period` every read below is answered at, in seconds, and how far either side of now its
/// window reaches — wide enough that the minute a batch was read in is inside it.
const PERIOD_SECS: i64 = 60;
/// See [`PERIOD_SECS`].
const WINDOW_SECS: f64 = 900.0;

/// How long an observation of work the CLOCK owes — the stack settling, a batch being read and
/// metered — waits before calling it a hang, and how long between two looks. Generous: a row fails
/// only when the work did not happen at all, so a loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(120);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How long before now a row's records are stamped as written. Several seconds, so every bound
/// below is a span the clock really ran rather than one a fast host could collapse.
const WRITTEN_AGO: Duration = Duration::from_secs(3);

/// One row of the table: how the records of its own table's stream were written, and how many of
/// them one batch holds.
struct Row {
    /// What a failed assertion names this row by.
    case: &'static str,
    /// Milliseconds into its second the row's writes are stamped at — the phase rounding a stamp
    /// down to the second discards — or `None` for the row written through DynamoDB's own
    /// `PutItem`, which lands on whatever phase the wall clock is at.
    phase_millis: Option<u64>,
    /// How many records the row writes, which is also the `BatchSize` its mapping is declared with.
    records: usize,
}

/// The rows: the phases a write can fall on, from the second's own boundary (where rounding the
/// stamp down loses nothing) to the last whole millisecond before the next second (where it loses
/// the most it can), crossed with how many records the batch holds — and the live wall clock's own
/// phase, through the path a client's own write takes.
const ROWS: &[Row] = &[
    Row {
        case: "on-the-second",
        phase_millis: Some(0),
        records: 1,
    },
    Row {
        case: "mid-second",
        phase_millis: Some(500),
        records: 2,
    },
    Row {
        case: "the-last-millisecond",
        phase_millis: Some(999),
        records: 3,
    },
    Row {
        case: "the-live-wall-clocks-own-phase",
        phase_millis: None,
        records: 1,
    },
];

/// The whole emulator over one engine, exactly as the binary assembles it, with the engine handed
/// back: a row that fixes the sub-second phase of a write makes it through the seam DynamoDB's own
/// item writes call, which is the only way a case can choose that phase.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    (engine, router)
}

/// The logical id, the table name, the function name and the stack output each row's resources are
/// declared under.
fn table_logical_id(index: usize) -> String {
    format!("SourceTable{index}")
}
/// See [`table_logical_id`].
fn table_name(index: usize) -> String {
    format!("metered-pipeline-source-{index}")
}
/// See [`table_logical_id`].
fn function_logical_id(index: usize) -> String {
    format!("Consumer{index}")
}
/// See [`table_logical_id`].
fn function_name(index: usize) -> String {
    format!("metered-pipeline-consumer-{index}")
}
/// See [`table_logical_id`].
fn mapping_logical_id(index: usize) -> String {
    format!("StreamTrigger{index}")
}
/// See [`table_logical_id`].
fn stream_arn_output(index: usize) -> String {
    format!("StreamArn{index}")
}

/// The trust and the permissions a function polling a table's stream is deployed with: what AWS
/// Lambda requires of a mapping onto a DynamoDB stream.
fn role() -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "RoleName": ROLE_NAME,
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {"Service": "lambda.amazonaws.com"},
                    "Action": "sts:AssumeRole",
                }],
            },
            "Policies": [{
                "PolicyName": "read-the-stream",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": [
                            "dynamodb:DescribeStream",
                            "dynamodb:GetRecords",
                            "dynamodb:GetShardIterator",
                            "dynamodb:ListStreams",
                            "logs:CreateLogGroup",
                            "logs:CreateLogStream",
                            "logs:PutLogEvents",
                        ],
                        "Resource": "*",
                    }],
                },
            }],
        },
    })
}

/// The template the estate is deployed from: one stream-enabled table, function and mapping per
/// row, the one role those functions report, and a dashboard over the series every row is read
/// through.
fn template() -> String {
    let mut resources = Map::new();
    resources.insert(ROLE.to_string(), role());
    let mut outputs = Map::new();
    let mut widgets: Vec<Value> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        resources.insert(
            table_logical_id(index),
            json!({
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": table_name(index),
                    "AttributeDefinitions": [
                        {"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"},
                    ],
                    "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
                    "BillingMode": "PAY_PER_REQUEST",
                    "StreamSpecification": {"StreamViewType": STREAM_VIEW_TYPE},
                },
            }),
        );
        resources.insert(
            function_logical_id(index),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Code": {"ZipFile": "def handler(event, context):\n    return event\n"},
                },
            }),
        );
        resources.insert(
            mapping_logical_id(index),
            json!({
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "EventSourceArn": {"Fn::GetAtt": [table_logical_id(index), "StreamArn"]},
                    "FunctionName": {"Ref": function_logical_id(index)},
                    "StartingPosition": STARTING_POSITION,
                    "BatchSize": row.records.max(1),
                },
            }),
        );
        outputs.insert(
            stream_arn_output(index),
            json!({"Value": {"Fn::GetAtt": [table_logical_id(index), "StreamArn"]}}),
        );
        widgets.push(json!({
            "type": "metric",
            "properties": {
                "metrics": [[NAMESPACE, ITERATOR_AGE, FUNCTION_DIMENSION, function_name(index)]],
                "region": REGION,
            },
        }));
    }
    resources.insert(
        DASHBOARD.to_string(),
        json!({
            "Type": "AWS::CloudWatch::Dashboard",
            "Properties": {
                "DashboardName": DASHBOARD_NAME,
                "DashboardBody": json!({"widgets": widgets}).to_string(),
            },
        }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Milliseconds from `from` to `to`, and 0 for a `to` that is not after `from`.
fn millis_between(from: SystemTime, to: SystemTime) -> f64 {
    to.duration_since(from).unwrap_or_default().as_secs_f64() * 1_000.0
}

/// Epoch seconds, as a query's window is stated.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Publish one `INSERT` of the item `id` onto `stream_arn` at `at` — the seam DynamoDB's own item
/// writes call, which is where a record's creation instant is stamped.
fn append_at(
    engine: &engine::InMemoryEngine,
    case: &str,
    stream_arn: &str,
    id: &str,
    at: SystemTime,
) {
    let keys: AttributeMap = [(KEY_ATTRIBUTE.to_string(), AttributeValue::S(id.to_string()))]
        .into_iter()
        .collect();
    engine
        .state::<StreamsState>()
        .lock()
        .append_record(
            stream_arn,
            &RecordAppend {
                event_name: OperationType::Insert,
                keys: keys.clone(),
                new_image: Some(keys),
                old_image: None,
            },
            at,
        )
        .unwrap_or_else(|rejected| {
            panic!("case {case:?}: the stream {stream_arn} must take the record, got {rejected}")
        });
}

/// The `IteratorAge` datapoints CloudWatch holds for `function`.
async fn iterator_age_datapoints(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> Vec<Value> {
    let now = epoch_secs(SystemTime::now());
    let answered = cw(
        router,
        case,
        REGION,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": ITERATOR_AGE,
            "Dimensions": [{ "Name": FUNCTION_DIMENSION, "Value": function }],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Maximum", "Minimum"],
            "Unit": MILLISECONDS,
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Wait until `function`'s series holds a datapoint, answering what it held and the instant the
/// read that found it was made — the reference the upper bound is taken against, so the bound is
/// what the clock had really run by then.
async fn datapoints_once_published(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> (Vec<Value>, SystemTime) {
    let started = Instant::now();
    while started.elapsed() < DEADLINE {
        let points = iterator_age_datapoints(router, case, function).await;
        let read_at = SystemTime::now();
        if !points.is_empty() {
            return (points, read_at);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!(
        "case {case:?}: {NAMESPACE}/{ITERATOR_AGE} dimensioned on \
         {FUNCTION_DIMENSION}={function} held no datapoint within {DEADLINE:?}"
    )
}

/// What a row's writes mean for the series it is then read through: the instant no part of the
/// record's wait can predate, and the wait it already had before any poller could read it.
struct Written {
    /// The upper bound's reference: the record was written at or after this instant, so its age at
    /// a later read can be no longer than the span from here to that read.
    at_or_after: SystemTime,
    /// The lower bound: the record had already waited this long when the write was in the stream
    /// and readable, and every read is after that.
    waited_millis: f64,
}

/// RULE: every `IteratorAge` datapoint of a mapping a TEMPLATE wired onto a table's stream lies
/// between the wait its batch's newest record already had when it became readable and the wait it
/// had when the datapoint was read — so the lag the declared dashboard's series shows is never
/// longer than the time really elapsed since that write, whatever sub-second phase the write fell
/// on, and never a value the stream never had.
#[tokio::test]
async fn the_age_a_deployed_pipeline_meters_is_bounded_by_the_write_it_came_from() {
    let case = "the recorded pipeline's dashboard series";
    let (engine, router) = emulator();
    let body = template();
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // The template names its own role, which is what these acknowledgements are for.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer HTTP 200, got {status} with {answered}"
    );
    let (reached, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    let (_, events) = cloudformation(&router, "DescribeStackEvents", &[("StackName", STACK)]).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reached} with \
         resource failures {:?} in {described}",
        elements(&events, "ResourceStatusReason")
    );

    // The stream each row's mapping was wired onto is the one the stack published from the
    // table's own attribute: what is written below is written to THAT stream.
    let outputs = outputs_of(&described);

    // Every row's records are written before any series is read, so one deployment serves the
    // whole table.
    let mut written: Vec<Written> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let stream_arn = outputs
            .get(&stream_arn_output(index))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the stack to publish the table's stream, got {outputs:?}",
                    row.case
                )
            });
        match row.phase_millis {
            // A phase a row CHOOSES: the records are stamped at a known instant in the past, so
            // the span from that instant is a bound this row knows exactly.
            Some(phase_millis) => {
                let at = floor_to_second(SystemTime::now() - WRITTEN_AGO)
                    + Duration::from_millis(phase_millis);
                for record in 0..row.records {
                    append_at(
                        &engine,
                        row.case,
                        &stream_arn,
                        &format!("{}-{record}", row.case),
                        at,
                    );
                }
                written.push(Written {
                    at_or_after: at,
                    waited_millis: millis_between(at, SystemTime::now()),
                });
            }
            // The phase NOBODY chooses: the item goes through DynamoDB's own write, which stamps
            // the record at or after this instant.
            None => {
                let before_write = SystemTime::now();
                let (status, put) = dynamodb(
                    &router,
                    "PutItem",
                    &json!({
                        "TableName": table_name(index),
                        "Item": {
                            KEY_ATTRIBUTE: {"S": row.case},
                            "payload": {"S": "written"},
                        },
                    }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: PutItem must answer HTTP 200, got {status} with {put}",
                    row.case
                );
                written.push(Written {
                    at_or_after: before_write,
                    waited_millis: 0.0,
                });
            }
        }
    }

    for (index, (row, written)) in ROWS.iter().zip(written.iter()).enumerate() {
        let function = function_name(index);
        let (points, read_at) = datapoints_once_published(&router, row.case, &function).await;
        let at_most = millis_between(written.at_or_after, read_at);
        for point in &points {
            let unit = point["Unit"].as_str().unwrap_or_default();
            assert_eq!(
                unit, MILLISECONDS,
                "case {:?}: the age must be reported in {MILLISECONDS}, got {point}",
                row.case
            );
            let largest = point["Maximum"].as_f64().unwrap_or_default();
            let smallest = point["Minimum"].as_f64().unwrap_or_default();
            assert!(
                largest <= at_most,
                "case {:?}: a batch of {} record(s) whose newest record was written at most \
                 {at_most} ms before the read cannot meter longer than that, got {largest} ms",
                row.case,
                row.records
            );
            assert!(
                smallest >= 0.0,
                "case {:?}: an age is never negative, got {smallest} ms",
                row.case
            );
            assert!(
                smallest >= written.waited_millis,
                "case {:?}: the record had already waited {} ms when it became readable, and \
                 every read is after that, got {smallest} ms",
                row.case,
                written.waited_millis
            );
        }
    }
}
