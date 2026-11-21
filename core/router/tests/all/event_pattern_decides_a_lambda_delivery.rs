// SPDX-License-Identifier: CC0-1.0

//! WHICH deliveries to an AWS Lambda function an EVENT PATTERN admits, over the assembled
//! emulator, read off the rows that function's own handler writes into an Amazon DynamoDB
//! table — the surface a client watches an event-driven pipeline through, and the surface
//! both recorded checks behind this suite watch it through.
//!
//! Two patterns decide a delivery to a function, and this suite pins one rule each.
//!
//! * A MAPPING'S `FilterCriteria`. "An object that defines the filter criteria that determine
//!   whether Lambda should process an event" (`FilterCriteria`), whose `Filters` is "a list of
//!   filters" — so of the records a poll takes off an Amazon DynamoDB table's stream exactly
//!   those matching AT LEAST ONE of the mapping's patterns are handed to the function, a record
//!   no pattern matches is handed to no invocation at all, and the mapping's position still
//!   advances past it so the records behind it are delivered. A mapping carrying no criteria
//!   determines nothing and admits every record.
//! * A RULE'S `EventPattern`. Every entry a publication ACCEPTS (`FailedEntryCount` 0 with one
//!   `EventId` per entry, which is all `PutEvents` models) is matched against each enabled rule
//!   on the bus it was published onto, and every target of a MATCHING rule whose own
//!   resource-based policy admits `events.amazonaws.com` `lambda:InvokeFunction` is handed that
//!   entry as the event envelope — `source` and `detail-type` as submitted and `detail` as the
//!   submitted JSON document — while an entry no rule's pattern matches is handed to nothing.
//!   None of that depends on how the rule, its target or the grant came to exist: an
//!   `AWS::Events::Rule` with its `Targets` and an `AWS::Lambda::Permission` leave behind the
//!   same rule, target and statement a client's own `PutRule`/`PutTargets`/`AddPermission` do,
//!   and they are live as soon as the stack reports itself created.
//!
//! They share this file because they are the same decision taken in two places: a pattern is
//! matched against one document, and a delivery the pattern does not admit is not made — so a
//! suite that pins one and not the other leaves half of the decision unobserved. Both halves
//! are read the same way, as the rows a handler wrote, so a withheld delivery cannot be
//! confused with a slow one: the row expected to be RECORDED is produced by the record (or the
//! entry of the same publication) that follows every withheld one, and a shard is read in
//! order, so once the recorded row is in the table the withheld ones have been read past and
//! their absence is a verdict rather than a race.
//!
//! Many crates have to agree for either rule to hold, and each handler's own SDK call is
//! dispatched back into this same router, so only the assembled emulator shows them end to
//! end: CloudFormation installs the wiring and archives the inline handler into a package this
//! emulator can run, IAM holds the execution role the poller reads the stream as and the
//! handler's own write is judged against, EventBridge matches an entry and hands its targets
//! over, Lambda holds the invoke grant, polls the stream and RUNS the handler, DynamoDB serves
//! both the streamed source and the table the rows land in.
//!
//! The mapping rows differ along what the first rule generalises over: which member of the
//! record the pattern names (a value nested inside the changed item's keys, the change's own
//! name), how many values one clause lists, whether the clause is an exact value or a content
//! filter, how many patterns the criteria carry (none, one, two that are ORed), and how much of
//! a batch they admit (all of it, some of it, none of it). The publication rows differ along
//! what the second generalises over: whether the entry's `source` and its `detail-type` are
//! ones the deployed rule's pattern names, and the shape of the `detail` document delivered (a
//! flat object, one carrying a nested object, an array, a boolean and non-ASCII text).
//!
//! The neighbouring suites, so the copies cannot drift apart: `all/ddb_stream_lambda_esm_filter_criteria`
//! and `all/ddb_stream_lambda_esm_filter_criteria_on_a_started_emulator` pin the first rule with
//! the deliveries read off an Amazon SQS queue and batches of ten, and
//! `all/cloudformation_events_rule_lambda_target_permission_boundary`,
//! `all/events_rule_lambda_target_direct_invoke_and_relay` and
//! `all/events_rule_lambda_target_writes_the_detail_into_dynamodb` pin the second over the grants
//! that decide a hand-over and over a rule a CLIENT installed. What is added here is what none of
//! them reaches: a filtered mapping whose batch holds ONE record, so a batch the criteria admit
//! nothing of is a batch with no invocation to make; a `prefix` content filter in a criteria
//! pattern; and the whole declared rule → target → table chain read back through the stack's own
//! output, with the submitted `detail` round-tripped through the handler.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The assembled router, the stack readers and every wire caller whose Region is this suite's come
// from the suites that own them rather than from second copies here, so this suite cannot drift
// from them. The one caller written below is EventBridge's ([`events`]), because the suite that
// publishes it signs for another Region than the one these stacks are deployed into.
use crate::cfn_ddb_stream_esm_lambda_pipeline::{create_failures, settled};
use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::dynamodb;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router as emulator};
use crate::logs_subscription_filter_to_lambda::lambda;
use crate::query_wire::outputs_of;

/// The `StackStatus` each stack below has to settle in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where Lambda binds the two collections used below.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `State` Lambda reports for a function an invocation may be made of, and for a mapping
/// that is reading its source.
const FUNCTION_ACTIVE: &str = "Active";
const MAPPING_ENABLED: &str = "Enabled";

/// The attribute every table below is keyed by, and the attribute a write to a streamed source
/// changes.
const KEY_ATTRIBUTE: &str = "id";
const VALUE_ATTRIBUTE: &str = "v";

/// The billing mode every declared and created table carries
/// (`CreateTable$BillingMode`: "one of: PAY_PER_REQUEST | PROVISIONED").
const PAY_PER_REQUEST: &str = "PAY_PER_REQUEST";

/// The `TableStatus` a table serves reads and writes in.
const TABLE_ACTIVE: &str = "ACTIVE";

/// The `StreamViewType` every source table is created with: both images, so every member a
/// pattern could name really is in the record.
const STREAM_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";

/// Where every mapping starts reading — the oldest record retained — so the writes made before
/// the mapping existed are the backlog its filter decides.
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// How many records one batch of the mappings below holds: ONE, so every record is a batch of
/// its own and a batch the criteria admit nothing of is a batch with no invocation to make.
const BATCH_SIZE: u64 = 1;

/// The `OperationType` a DynamoDB stream record reports for each kind of change
/// (`smithy_query(query="shape", shape="OperationType")`, DynamoDB Streams).
const INSERT: &str = "INSERT";
/// See [`INSERT`].
const MODIFY: &str = "MODIFY";
/// See [`INSERT`].
const REMOVE: &str = "REMOVE";

/// AWS's own published execution-role policy for a function that reads a DynamoDB stream: it
/// grants the stream reads Lambda's poller makes on the function's behalf.
const DYNAMODB_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaDynamoDBExecutionRole";

/// The `python3.x` runtime every function here declares.
const RUNTIME: &str = "python3.12";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is which deliveries are made and not how fast
/// an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The service principal EventBridge hands a resource-policy-governed target over as, and the
/// action an invocation of a function performs ("for Lambda functions, Amazon SNS topics and
/// Amazon SQS queues, EventBridge relies on resource-based policies", `PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `source` the deployed rule's pattern names, and one it does not.
const MATCHED_SOURCE: &str = "com.example.orders.deployed";
const OTHER_SOURCE: &str = "com.example.orders.elsewhere";

/// The `detail-type` that rule's pattern names, and one it does not.
const MATCHED_DETAIL_TYPE: &str = "order-created";
const OTHER_DETAIL_TYPE: &str = "order-cancelled";

/// The member of a published `Detail` the rule's target records its row under.
const MARKER_MEMBER: &str = "test_marker";

/// How long ONE test waits, in all, for the work the clock owes it — a stack settling, a table
/// reaching `ACTIVE`, a function going `Active`, a mapping reaching the state it reads in, a
/// delivery and the write it makes. One deadline is taken at the start of a test and every wait
/// in it looks again until its condition holds or that instant passes, so a test that will fail
/// is bounded by this span rather than by the sum of its waits.
///
/// A GENEROUS wall-clock ceiling rather than an expectation: the spans the services behind this
/// wiring choose take the time they take, so a loaded host cannot decide an outcome, and all
/// the ceiling is for is that a wait TERMINATES. Nothing here asserts on elapsed time, and the
/// work the rows wait on is STARTED together (see the first test), so the ceiling covers one
/// round of those spans rather than their sum.
const WAIT: Duration = Duration::from_secs(240);
/// See [`WAIT`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler the filtered mapping's function runs: for each record it is HANDED it writes one
/// row into the observation table, keyed by the change the record reports and the key of the
/// item that changed, both taken out of the record itself — so a row can only exist for a
/// record that reached the handler.
const RECORDING_HANDLER: &str = r##"
import os

import boto3

OBSERVATION_TABLE = os.environ["OBSERVATION_TABLE"]
KEY_ATTRIBUTE = os.environ["KEY_ATTRIBUTE"]
CLIENT = boto3.client("dynamodb")


def handler(event, context):
    recorded = []
    for record in event["Records"]:
        key = record["dynamodb"]["Keys"][KEY_ATTRIBUTE]["S"]
        change = record["eventName"]
        marker = change + "#" + key
        CLIENT.put_item(
            TableName=OBSERVATION_TABLE,
            Item={
                "id": {"S": marker},
                "change": {"S": change},
                "source_key": {"S": key},
            },
        )
        recorded.append(marker)
    return {"recorded": recorded}
"##;

/// The handler the deployed rule's target runs: for the envelope it is handed it writes one row
/// into the table the same stack declares, keyed by the marker the event's own `detail` carries,
/// beside the envelope members the delivery brought and the whole `detail` document.
const TARGET_HANDLER: &str = r##"
import json
import os

import boto3

TARGET_TABLE = os.environ["TARGET_TABLE"]
MARKER_MEMBER = os.environ["MARKER_MEMBER"]
CLIENT = boto3.client("dynamodb")


def handler(event, context):
    detail = event["detail"]
    marker = detail[MARKER_MEMBER]
    CLIENT.put_item(
        TableName=TARGET_TABLE,
        Item={
            "id": {"S": marker},
            "source": {"S": event["source"]},
            "detail_type": {"S": event["detail-type"]},
            "payload": {"S": json.dumps(detail)},
        },
    )
    return {"recorded": marker}
"##;

/// What one write does to the item it names.
#[derive(Debug, Clone, Copy)]
enum Change {
    /// A key the table does not hold yet, which the stream reports as an `INSERT`.
    Insert,
    /// A key it holds, written with another value, which it reports as a `MODIFY`.
    Overwrite,
    /// A key it holds, deleted, which it reports as a `REMOVE`.
    Delete,
}

/// One mapping row: the patterns its criteria carry, the writes made to its own source table
/// before it exists, and which of the records those writes produce the handler must and must
/// not be handed.
struct Filtered {
    case: &'static str,
    /// The `FilterCriteria.Filters[].Pattern` values, in order; empty for a mapping carrying no
    /// criteria at all.
    patterns: &'static [&'static str],
    /// The key and the change of each write, in the order they are made.
    writes: &'static [(&'static str, Change)],
    /// The records the handler must be handed, as `(change, key)`; the LAST of them is produced
    /// by a write made after every withheld one.
    recorded: &'static [(&'static str, &'static str)],
    /// The records no invocation may be handed, as `(change, key)`.
    withheld: &'static [(&'static str, &'static str)],
}

/// The mapping rows, each with its own source table and its own keys, so no row can read or
/// write another's.
const FILTERED: &[Filtered] = &[
    // The recorded shape: one clause on a value NESTED inside the changed item's keys.
    Filtered {
        case: "a-clause-on-the-changed-items-key",
        patterns: &[r#"{"dynamodb":{"Keys":{"id":{"S":["a-keep"]}}}}"#],
        writes: &[("a-skip", Change::Insert), ("a-keep", Change::Insert)],
        recorded: &[(INSERT, "a-keep")],
        withheld: &[(INSERT, "a-skip")],
    },
    // A clause on the change's own name, listing two values of it.
    Filtered {
        case: "two-values-of-the-changes-own-name",
        patterns: &[r#"{"eventName":["MODIFY","REMOVE"]}"#],
        writes: &[("b-1", Change::Insert), ("b-1", Change::Overwrite)],
        recorded: &[(MODIFY, "b-1")],
        withheld: &[(INSERT, "b-1")],
    },
    // Two patterns that are ORed, the second a content filter rather than an exact value: a
    // record matching EITHER is handed over.
    Filtered {
        case: "two-patterns-one-of-them-a-prefix",
        patterns: &[
            r#"{"eventName":["REMOVE"]}"#,
            r#"{"dynamodb":{"Keys":{"id":{"S":[{"prefix":"c-keep"}]}}}}"#,
        ],
        writes: &[
            ("c-skip", Change::Insert),
            ("c-keep-1", Change::Insert),
            ("c-skip", Change::Delete),
        ],
        recorded: &[(INSERT, "c-keep-1"), (REMOVE, "c-skip")],
        withheld: &[(INSERT, "c-skip")],
    },
    // No criteria at all: the mapping determines nothing, so every record is handed over.
    Filtered {
        case: "no-criteria-admits-every-record",
        patterns: &[],
        writes: &[("d-1", Change::Insert), ("d-1", Change::Overwrite)],
        recorded: &[(INSERT, "d-1"), (MODIFY, "d-1")],
        withheld: &[],
    },
    // Batches the criteria admit NOTHING of, each one record of its own: no invocation is made
    // for them and the position still advances, which the record behind them is the proof of.
    Filtered {
        case: "batches-the-criteria-admit-nothing-of",
        patterns: &[r#"{"eventName":["REMOVE"]}"#],
        writes: &[
            ("e-1", Change::Insert),
            ("e-1", Change::Overwrite),
            ("e-1", Change::Delete),
        ],
        recorded: &[(REMOVE, "e-1")],
        withheld: &[(INSERT, "e-1"), (MODIFY, "e-1")],
    },
];

/// One published entry: what it carries, and whether the deployed rule's pattern names it.
struct Published {
    case: &'static str,
    source: &'static str,
    detail_type: &'static str,
    /// The marker the entry's `detail` carries, which is the key the target's row is written
    /// under.
    marker: &'static str,
    /// The whole `Detail` document the entry publishes, carrying that marker.
    detail: Value,
    /// Whether the rule's pattern names this entry, so its target is handed it.
    delivered: bool,
}

/// The entries ONE publication carries: the two the pattern does not name first, so a delivery
/// of either would stand beside the rows the two it does name produce.
fn published() -> Vec<Published> {
    vec![
        Published {
            case: "a source the pattern does not name",
            source: OTHER_SOURCE,
            detail_type: MATCHED_DETAIL_TYPE,
            marker: "evt-other-source",
            detail: json!({ MARKER_MEMBER: "evt-other-source", "amount": 7 }),
            delivered: false,
        },
        Published {
            case: "a detail-type the pattern does not name",
            source: MATCHED_SOURCE,
            detail_type: OTHER_DETAIL_TYPE,
            marker: "evt-other-detail-type",
            detail: json!({ MARKER_MEMBER: "evt-other-detail-type", "amount": 11 }),
            delivered: false,
        },
        Published {
            case: "the recorded shape: a flat detail the pattern names",
            source: MATCHED_SOURCE,
            detail_type: MATCHED_DETAIL_TYPE,
            marker: "evt-flat-detail",
            detail: json!({ MARKER_MEMBER: "evt-flat-detail", "amount": 42 }),
            delivered: true,
        },
        Published {
            case: "a detail carrying a nested object, an array, a boolean and non-ASCII text",
            source: MATCHED_SOURCE,
            detail_type: MATCHED_DETAIL_TYPE,
            marker: "evt-nested-detail",
            detail: json!({
                MARKER_MEMBER: "evt-nested-detail",
                "amount": 0,
                "items": ["first", "second"],
                "nested": { "label": "naïve — ünicode", "express": true },
            }),
            delivered: true,
        },
    ]
}

/// The trust policy every execution role below carries: Lambda has to be able to assume the
/// role for a function to be handed it at all.
fn lambda_trust() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One DynamoDB call that must answer 200.
async fn ddb(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, answered) = dynamodb(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {answered}"
    );
    answered
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
///
/// Written here rather than imported because the sibling that publishes this caller signs for
/// another Region, and the rule and the function below belong to the Region these stacks are
/// deployed into: a publication signed elsewhere would reach another Region's buses.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("authorization", authorization("events", REGION))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// Deploy `template` as `stack` and answer the outputs it published, reporting which declared
/// resource was refused when it does not settle created.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    giving_up_at: Instant,
) -> BTreeMap<String, String> {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            // The template declares an IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let (settled_status, described) = settled(router, stack, giving_up_at).await;
    let failures = if settled_status == CREATE_COMPLETE {
        Vec::new()
    } else {
        create_failures(router, stack).await
    };
    assert_eq!(
        settled_status, CREATE_COMPLETE,
        "case {case:?}: expected the stack to settle {CREATE_COMPLETE}, got {described} with \
         refused resources {failures:?}"
    );
    outputs_of(&described)
}

/// The output `name` the stack published, which every test below needs to address what was
/// deployed the way a client addresses it.
fn output(case: &str, outputs: &BTreeMap<String, String>, name: &str) -> String {
    outputs.get(name).cloned().unwrap_or_else(|| {
        panic!("case {case:?}: expected the stack to publish {name}, got {outputs:?}")
    })
}

/// Wait until Lambda reports `function` `Active` — "the function is ready to be invoked"
/// (`FunctionConfiguration$State`) — which is the state both deliveries below need.
async fn wait_until_active(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    giving_up_at: Instant,
) {
    loop {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await
        .json();
        if read.get("State").and_then(Value::as_str) == Some(FUNCTION_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: Lambda never reported {function:?} {FUNCTION_ACTIVE}, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The row `id` of `table` holds right now, through the read a client makes of it.
async fn row_under(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    id: &str,
) -> Option<Value> {
    let answered = ddb(
        router,
        case,
        "GetItem",
        &json!({
            "TableName": table,
            "Key": { KEY_ATTRIBUTE: { "S": id } },
            "ConsistentRead": true,
        }),
    )
    .await;
    answered.get("Item").cloned()
}

/// Look again until `table` holds the row under `id`, answering it.
async fn awaited_row(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    id: &str,
    giving_up_at: Instant,
) -> Value {
    loop {
        if let Some(row) = row_under(router, case, table, id).await {
            return row;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected a handler to be handed the delivery under {id:?} and to \
             write its row into {table:?}, no row is there"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Ask for one stream-enabled source table, without waiting for it: every row's table is asked
/// for before any of them is waited out, so the rows wait through ONE round of the span a table
/// takes to come up rather than one round each.
async fn create_stream_table(router: &MultiServiceRouter, case: &str, name: &str) {
    ddb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": name,
            "AttributeDefinitions": [{ "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
            "BillingMode": PAY_PER_REQUEST,
            "StreamSpecification": { "StreamEnabled": true, "StreamViewType": STREAM_VIEW_TYPE },
        }),
    )
    .await;
}

/// The ARN of the stream `name` minted, once that table reports itself serving.
async fn awaited_stream(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    giving_up_at: Instant,
) -> String {
    loop {
        let described = ddb(router, case, "DescribeTable", &json!({ "TableName": name })).await;
        let active =
            described.pointer("/Table/TableStatus").and_then(Value::as_str) == Some(TABLE_ACTIVE);
        let minted = described
            .pointer("/Table/LatestStreamArn")
            .and_then(Value::as_str);
        if let (true, Some(stream)) = (active, minted) {
            return stream.to_string();
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the table {name:?} never reached {TABLE_ACTIVE} with a stream, last \
             read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Make one write of a row against its source table.
async fn write_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &str,
    change: Change,
) {
    let (operation, request) = match change {
        Change::Insert => (
            "PutItem",
            json!({
                "TableName": table,
                "Item": { KEY_ATTRIBUTE: { "S": key }, VALUE_ATTRIBUTE: { "S": "first" } },
            }),
        ),
        Change::Overwrite => (
            "PutItem",
            json!({
                "TableName": table,
                "Item": { KEY_ATTRIBUTE: { "S": key }, VALUE_ATTRIBUTE: { "S": "second" } },
            }),
        ),
        Change::Delete => (
            "DeleteItem",
            json!({ "TableName": table, "Key": { KEY_ATTRIBUTE: { "S": key } } }),
        ),
    };
    ddb(router, case, operation, &request).await;
}

/// Ask for one row's mapping over `stream`, carrying that row's patterns when it has any,
/// without waiting for it to start reading — answered as the id Lambda minted for it.
///
/// The create is asserted on the way: `CreateEventSourceMapping` answers 202 and echoes the
/// criteria it was given back on the configuration, and a mapping carrying none reports none.
async fn create_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream: &str,
    patterns: &[&str],
) -> String {
    let criteria = json!({
        "Filters": patterns
            .iter()
            .map(|pattern| json!({ "Pattern": pattern }))
            .collect::<Vec<Value>>(),
    });
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream,
        "StartingPosition": STARTING_POSITION,
        "BatchSize": BATCH_SIZE,
        "Enabled": true,
    });
    if !patterns.is_empty() {
        request["FilterCriteria"] = criteria.clone();
    }
    let answered = lambda(router, "POST", MAPPINGS_URI, Some(request)).await;
    assert_eq!(
        answered.status,
        202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {} with body {:?}",
        answered.status,
        answered.text()
    );
    let created = answered.json();
    if patterns.is_empty() {
        let reported = created.get("FilterCriteria");
        assert!(
            reported.is_none_or(Value::is_null),
            "case {case:?}: expected a mapping carrying no criteria to report none, got {created}"
        );
    } else {
        assert_eq!(
            created.get("FilterCriteria"),
            Some(&criteria),
            "case {case:?}: expected the create to echo the criteria {criteria}, got {created}"
        );
    }
    created
        .get("UUID")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the created mapping reports no UUID, got {created}")
        })
        .to_string()
}

/// Look again until Lambda reports the mapping `uuid` reading its source, which is the state a
/// record is handed over in.
async fn wait_until_reading(
    router: &MultiServiceRouter,
    case: &str,
    uuid: &str,
    giving_up_at: Instant,
) {
    loop {
        let read = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None)
            .await
            .json();
        if read.get("State").and_then(Value::as_str) == Some(MAPPING_ENABLED) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the mapping {uuid:?} never reported {MAPPING_ENABLED}, last read \
             {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// How this suite names one record a mapping's function was handed: the change it reported and
/// the key it changed.
fn marker(change: &str, key: &str) -> String {
    format!("{change}#{key}")
}

/// Whether a policy member that may be either one value or a list of them names `wanted` — the
/// form an IAM `Action` or a `Principal`'s service takes.
fn names(member: &Value, wanted: &str) -> bool {
    match member {
        Value::String(one) => one == wanted,
        Value::Array(many) => many.iter().any(|one| one == wanted),
        _ => false,
    }
}

/// The observation table, the execution role and the ONE recording function every mapping row
/// below hands its records to, as one template — the inline source is archived into the
/// function's deployment package by CloudFormation, which is how a function whose handler is
/// written here comes to exist at all.
fn consumer_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a DynamoDB-stream consumer and the table it records what it was handed in",
        "Resources": {
            "ObservationTable": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "AttributeDefinitions": [
                        { "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }
                    ],
                    "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
                    "BillingMode": PAY_PER_REQUEST,
                },
            },
            "ConsumerRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": lambda_trust(),
                    "ManagedPolicyArns": [DYNAMODB_EXECUTION_POLICY],
                    "Policies": [{
                        "PolicyName": "record-what-was-handed-over",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "dynamodb:PutItem",
                                "Resource": { "Fn::GetAtt": ["ObservationTable", "Arn"] },
                            }],
                        },
                    }],
                },
            },
            "Consumer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["ConsumerRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {
                            "OBSERVATION_TABLE": { "Ref": "ObservationTable" },
                            "KEY_ATTRIBUTE": KEY_ATTRIBUTE,
                        },
                    },
                    "Code": { "ZipFile": RECORDING_HANDLER },
                },
            },
        },
        "Outputs": {
            "ObservationTableName": { "Value": { "Ref": "ObservationTable" } },
            "FunctionName": { "Value": { "Ref": "Consumer" } },
        },
    })
    .to_string()
}

/// The canonical deployment of an event-driven function as one template: the table the target
/// records into, its execution role, the function, the rule whose `EventPattern` names a source
/// and a detail-type with that function as its target, and the `AWS::Lambda::Permission` that
/// grants EventBridge the invocation of it.
fn rule_target_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an EventBridge rule whose Lambda target records what it was handed",
        "Resources": {
            "TargetTable": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "AttributeDefinitions": [
                        { "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }
                    ],
                    "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
                    "BillingMode": PAY_PER_REQUEST,
                },
            },
            "TargetRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": lambda_trust(),
                    "Policies": [{
                        "PolicyName": "record-the-delivered-detail",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "dynamodb:PutItem",
                                "Resource": { "Fn::GetAtt": ["TargetTable", "Arn"] },
                            }],
                        },
                    }],
                },
            },
            "Target": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["TargetRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {
                            "TARGET_TABLE": { "Ref": "TargetTable" },
                            "MARKER_MEMBER": MARKER_MEMBER,
                        },
                    },
                    "Code": { "ZipFile": TARGET_HANDLER },
                },
            },
            "Rule": {
                "Type": "AWS::Events::Rule",
                "Properties": {
                    "EventPattern": {
                        "source": [MATCHED_SOURCE],
                        "detail-type": [MATCHED_DETAIL_TYPE],
                    },
                    "State": "ENABLED",
                    "Targets": [{
                        "Id": "recording-target",
                        "Arn": { "Fn::GetAtt": ["Target", "Arn"] },
                    }],
                },
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Fn::GetAtt": ["Target", "Arn"] },
                    "Action": INVOKE_ACTION,
                    "Principal": EVENTS_PRINCIPAL,
                    "SourceArn": { "Fn::GetAtt": ["Rule", "Arn"] },
                },
            },
        },
        "Outputs": {
            "TableName": { "Value": { "Ref": "TargetTable" } },
            "FunctionName": { "Value": { "Ref": "Target" } },
            "RuleName": { "Value": { "Ref": "Rule" } },
            "RuleArn": { "Value": { "Fn::GetAtt": ["Rule", "Arn"] } },
        },
    })
    .to_string()
}

/// RULE: of the records a DynamoDB-stream mapping polls, exactly those at least one of its
/// `FilterCriteria` patterns matches are handed to its function; a record no pattern matches is
/// handed to no invocation while the mapping's position still advances past it, and a mapping
/// carrying no criteria admits every record.
#[tokio::test]
async fn filter_criteria_decide_which_stream_records_a_function_is_handed() {
    let router = emulator();
    let giving_up_at = Instant::now() + WAIT;
    let stack = "xsvc-pattern-filtered-consumer";

    let outputs = deployed(
        &router,
        "the filtered consumer's deployment",
        stack,
        &consumer_template(),
        giving_up_at,
    )
    .await;
    let observation = output(
        "the filtered consumer's deployment",
        &outputs,
        "ObservationTableName",
    );
    let function = output(
        "the filtered consumer's deployment",
        &outputs,
        "FunctionName",
    );
    wait_until_active(
        &router,
        "the filtered consumer's deployment",
        &function,
        giving_up_at,
    )
    .await;

    // Every row's own source table is ASKED FOR before any of them is waited out, and then
    // every row's mapping is asked for before any of them is waited out: the spans those two
    // creates take are the clock's, so starting them together has the rows wait through one
    // round of each rather than one round per row. Nothing about a row changes — each still
    // writes its own keys to its own table before its own mapping exists.
    let sources: Vec<String> = (0..FILTERED.len())
        .map(|index| format!("{stack}-source-{index}"))
        .collect();
    for (row, source) in FILTERED.iter().zip(&sources) {
        create_stream_table(&router, row.case, source).await;
    }
    let mut streams: Vec<String> = Vec::with_capacity(FILTERED.len());
    for (row, source) in FILTERED.iter().zip(&sources) {
        streams.push(awaited_stream(&router, row.case, source, giving_up_at).await);
    }

    // Every write is made BEFORE its row's mapping exists, which is the shape a client wires a
    // projection in: a mapping reading from the oldest retained record reads the backlog, so
    // the criteria decide records that are already in the shard.
    for (row, source) in FILTERED.iter().zip(&sources) {
        for (key, change) in row.writes {
            write_item(&router, row.case, source, key, *change).await;
        }
    }
    let mut mappings: Vec<String> = Vec::with_capacity(FILTERED.len());
    for (row, stream) in FILTERED.iter().zip(&streams) {
        mappings.push(create_mapping(&router, row.case, &function, stream, row.patterns).await);
    }
    for (row, uuid) in FILTERED.iter().zip(&mappings) {
        wait_until_reading(&router, row.case, uuid, giving_up_at).await;
    }

    for row in FILTERED {
        let case = row.case;
        for (change, key) in row.recorded {
            let id = marker(change, key);
            let row_held = awaited_row(&router, case, &observation, &id, giving_up_at).await;
            assert_eq!(
                row_held,
                json!({
                    KEY_ATTRIBUTE: { "S": id.clone() },
                    "change": { "S": *change },
                    "source_key": { "S": *key },
                }),
                "case {case:?}: expected the handler's row for the {change} record of {key:?}, \
                 got {row_held}"
            );
        }
        // Every withheld record was written BEFORE the last recorded one, and a shard is read
        // in order, so by now it has been read past: no row for it is a verdict.
        for (change, key) in row.withheld {
            let id = marker(change, key);
            let row_held = row_under(&router, case, &observation, &id).await;
            assert!(
                row_held.is_none(),
                "case {case:?}: no pattern of {:?} matches the {change} record of {key:?}, so no \
                 invocation may be handed it, but {observation:?} holds {row_held:?}",
                row.patterns
            );
        }
    }
}

/// RULE: once a stack declaring a rule, its Lambda target and the permission that admits
/// `events.amazonaws.com` reports itself created, that wiring is live — a publication whose
/// entry the rule's `EventPattern` names is accepted and handed to the target as the event
/// envelope, so the `detail` the entry carried reaches the handler and a later read of the
/// table it writes reports it, while an entry the pattern does not name is handed to nothing.
#[tokio::test]
async fn a_deployed_rules_lambda_target_records_the_detail_its_pattern_admits() {
    let case = "a deployed rule, its Lambda target and the permission that admits EventBridge";
    let router = emulator();
    let giving_up_at = Instant::now() + WAIT;
    let stack = "xsvc-pattern-rule-target";

    let outputs = deployed(&router, case, stack, &rule_target_template(), giving_up_at).await;
    let table = output(case, &outputs, "TableName");
    let function = output(case, &outputs, "FunctionName");
    let rule = output(case, &outputs, "RuleName");
    let rule_arn = output(case, &outputs, "RuleArn");
    wait_until_active(&router, case, &function, giving_up_at).await;

    // The declared target and the declared grant are what a client reads back: the rule really
    // carries that function as a target, and the function's own policy really admits this
    // service the invocation.
    let targets = events(&router, case, "ListTargetsByRule", &json!({ "Rule": rule })).await;
    let named: Vec<String> = targets["Targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|target| target["Arn"].as_str().map(str::to_string))
        .collect();
    assert!(
        named
            .iter()
            .any(|arn| arn.ends_with(&format!(":function:{function}"))),
        "case {case:?}: expected the deployed rule to target the deployed function {function:?}, \
         got {named:?}"
    );
    let policy = lambda(
        &router,
        "GET",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        None,
    )
    .await;
    let document: Value =
        serde_json::from_str(policy.json()["Policy"].as_str().unwrap_or_default())
            .unwrap_or(Value::Null);
    let statements = document["Statement"].as_array().cloned().unwrap_or_default();
    assert!(
        statements.iter().any(|statement| {
            statement["Effect"] == json!("Allow")
                && names(&statement["Action"], INVOKE_ACTION)
                && names(&statement["Principal"]["Service"], EVENTS_PRINCIPAL)
                && statement["Condition"]["ArnLike"]["AWS:SourceArn"] == json!(rule_arn)
        }),
        "case {case:?}: expected the function's policy to carry an Allow of {INVOKE_ACTION} for \
         {EVENTS_PRINCIPAL} scoped to the deployed rule {rule_arn:?}, got {document}"
    );

    // ONE publication carrying every row, the entries the pattern does not name first, so the
    // matched and the unmatched entries of the same request are dispatched together and the
    // absence of a row for an unmatched one is a verdict rather than a race.
    let rows = published();
    let entries: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "Source": row.source,
                "DetailType": row.detail_type,
                "Detail": row.detail.to_string(),
            })
        })
        .collect();
    let answered = events(&router, case, "PutEvents", &json!({ "Entries": entries })).await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected every entry to be accepted, got {answered}"
    );
    let accepted = answered["Entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        accepted.len(),
        rows.len(),
        "case {case:?}: expected one answered entry per published entry, got {answered}"
    );
    for (row, entry) in rows.iter().zip(&accepted) {
        assert!(
            entry["EventId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}/{:?}: expected the accepted entry to carry an EventId, got {entry}",
            row.case
        );
    }

    for row in rows.iter().filter(|row| row.delivered) {
        let case = row.case;
        let held = awaited_row(&router, case, &table, row.marker, giving_up_at).await;
        assert_eq!(
            held[KEY_ATTRIBUTE],
            json!({ "S": row.marker }),
            "case {case:?}: expected the row to be keyed by the detail's marker {:?}, got {held}",
            row.marker
        );
        assert_eq!(
            held["source"],
            json!({ "S": row.source }),
            "case {case:?}: expected the envelope's source {:?} to reach the handler, got {held}",
            row.source
        );
        assert_eq!(
            held["detail_type"],
            json!({ "S": row.detail_type }),
            "case {case:?}: expected the envelope's detail-type {:?} to reach the handler, got \
             {held}",
            row.detail_type
        );
        let delivered: Value = serde_json::from_str(held["payload"]["S"].as_str().unwrap_or(""))
            .unwrap_or(Value::Null);
        assert_eq!(
            delivered, row.detail,
            "case {case:?}: expected the submitted detail {} to reach the handler, got \
             {delivered}",
            row.detail
        );
    }
    for row in rows.iter().filter(|row| !row.delivered) {
        let case = row.case;
        let held = row_under(&router, case, &table, row.marker).await;
        assert!(
            held.is_none(),
            "case {case:?}: the rule's pattern does not name this entry, so its target may not \
             be handed it, but {table:?} holds {held:?}"
        );
    }
}
