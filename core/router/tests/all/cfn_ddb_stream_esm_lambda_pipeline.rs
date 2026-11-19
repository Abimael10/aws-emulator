// SPDX-License-Identifier: CC0-1.0

//! What a DECLARED DynamoDB-stream trigger's function does with the records it is handed, and
//! what a stack reports when a declared resource's own create is refused.
//!
//! RULE: the function an `AWS::Lambda::EventSourceMapping` names is handed the records of the
//! event source the mapping declares, and its own AWS SDK calls are made as a session of the
//! execution role the function declares — so a handler the stack deployed writes a SECOND
//! declared resource of the same stack under the grant the role's inline statement carries,
//! and the write is in that resource afterwards; and where the service that OWNS a declared
//! resource refuses its create, the stack records a `CREATE_FAILED` event for THAT resource
//! carrying that service's own refusal as its `ResourceStatusReason` before settling
//! `ROLLBACK_COMPLETE`, so a client reads which resource failed and why rather than a bare
//! terminal status.
//!
//! Six crates have to agree for that to hold, and the handler's own SDK call is dispatched
//! back into this same router, so only the assembled emulator shows it end to end.
//! `all/cloudformation_ddb_stream_lambda_trigger_estate` is the nearest neighbour and owns the
//! deploying half of this estate: that the stack and each of its resources reach
//! `CREATE_COMPLETE` over every `StreamViewType`, both billing modes and tables with and
//! without a sort key, that the `StreamArn` it exports is the live one, and that a write to
//! the deployed table reaches the deployed function at all.
//! `all/ddb_stream_lambda_esm_provisioning` pins that a stream-enabled table's stream is
//! mappable through each service's own API, and
//! `all/cloudformation_provisions_lambda_event_source_mappings` pins a declared mapping over
//! the sources that are not a table's stream. None of them is repeated here.
//!
//! What is pinned HERE is what those do not reach:
//!
//! * the handler's own SDK WRITE into another declared resource — the sink table the role's
//!   inline `dynamodb:PutItem` on `Fn::GetAtt SinkTable.Arn` grants — carrying each record's
//!   key and the change the record reports, which is the shape a client wires a stream
//!   projection in. The writes differ along what the rule generalises over: a key the table
//!   did not hold, the same key written again, and a second key, so the records the handler is
//!   handed differ in their change and in their key;
//! * the refusal half, which no suite pinned: the mapping's own create refused by the service
//!   that owns it, AWS Lambda, over the stream reads the declared execution role cannot make —
//!   asserted on the logical resource the `CREATE_FAILED` event names, on that being the only
//!   resource reported failed, on that service's own sentence, and on the stack settling
//!   `ROLLBACK_COMPLETE`.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// Every wire caller, the assembled router and the mapping vocabulary come from the suites that
// own them rather than from second copies here, so this suite cannot drift from them.
use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_dynamodb_table_in_place_update::{described_table, dynamodb};
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::cloudformation_provisions_lambda_event_source_mappings::{
    MAPPINGS_URI, MAPPING_STATE_ENABLED,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::router as emulator;
use crate::logs_subscription_filter_to_lambda::lambda;
use crate::query_wire::{block_of, members_of, outputs_of, physical_ids_of};

/// The account this emulator serves, which every ARN it mints carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `ResourceStatus` a refused resource create is recorded under.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical ids every template below declares its resources under.
const SOURCE_TABLE: &str = "SourceTable";
const SINK_TABLE: &str = "SinkTable";
const FUNCTION_ROLE: &str = "FunctionRole";
const STREAM_FUNCTION: &str = "StreamFunction";
const MAPPING: &str = "StreamEventSourceMapping";

/// The attributes the two tables are keyed by, and the attribute a write changes.
const KEY_ATTRIBUTE: &str = "pk";
const SINK_KEY_ATTRIBUTE: &str = "id";
const VALUE_ATTRIBUTE: &str = "v";

/// The billing mode the source table is declared with
/// (`CreateTable$BillingMode`: "one of: PAY_PER_REQUEST | PROVISIONED").
const PAY_PER_REQUEST: &str = "PAY_PER_REQUEST";

/// The two changes the writes below make to the source table, as the stream names them.
const INSERT: &str = "INSERT";
const MODIFY: &str = "MODIFY";

/// AWS's own published execution-role policy for a function that reads a DynamoDB stream,
/// which grants the stream reads Lambda's poller makes on the function's behalf, and the basic
/// one, which grants only the log writes — the contrast the first refusal row is stated on.
const DYNAMODB_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaDynamoDBExecutionRole";
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The `python3.x` runtime every function here declares.
const RUNTIME: &str = "python3.12";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the wiring and not how fast an interpreter
/// gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long ONE test waits, in all, for the work the clock owes it — a stack settling, a
/// mapping's poller reaching the state it reads in, a table reaching `ACTIVE`, the delivery of
/// a batch. One deadline is taken at the start of a test and every wait in it looks again
/// until the condition holds or that instant passes, so a test that will fail is bounded by
/// this span rather than by the sum of its waits.
///
/// A generous WALL-CLOCK deadline rather than a count of reads: the spans the services behind a
/// stack choose take the time they take, so a loaded host cannot decide the outcome, and all
/// the deadline is for is that the wait TERMINATES. Nothing here asserts on elapsed time.
const WAIT: Duration = Duration::from_secs(150);
/// See [`WAIT`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler the template archives into the function's deployment package: for each record
/// of the batch it was handed, it writes the record's own key and change name into the sink
/// table with the SDK the runtime supplies.
const SINK_HANDLER: &str = r##"
import os

import boto3

SINK_TABLE = os.environ["SINK_TABLE"]
KEY_ATTRIBUTE = os.environ["KEY_ATTRIBUTE"]
CLIENT = boto3.client("dynamodb")


def handler(event, context):
    written = []
    for record in event["Records"]:
        key = record["dynamodb"]["Keys"][KEY_ATTRIBUTE]["S"]
        event_name = record["eventName"]
        CLIENT.put_item(
            TableName=SINK_TABLE,
            Item={
                "id": {"S": key + "#" + event_name},
                "source_key": {"S": key},
                "event_name": {"S": event_name},
            },
        )
        written.append(key)
    return {"written": written}
"##;

/// The writes the delivery test makes to the source table, in order, as `(key, value)`: a key
/// the table does not hold yet, the same key again with another value, and a second key — so
/// the handler is handed records differing both in the change they report and in their key.
const WRITES: &[(&str, &str)] = &[("widget-a", "one"), ("widget-a", "two"), ("widget-b", "one")];

/// The source table's declaration: the stream its records come from, and the name it is
/// declared under where the test needs that name computable from the declaration rather than
/// read back out of the answer under test.
fn source_table(name: Option<&str>) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "AttributeDefinitions".to_string(),
        json!([{ "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }]),
    );
    properties.insert(
        "KeySchema".to_string(),
        json!([{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }]),
    );
    properties.insert("BillingMode".to_string(), json!(PAY_PER_REQUEST));
    properties.insert(
        "StreamSpecification".to_string(),
        json!({ "StreamViewType": "NEW_AND_OLD_IMAGES" }),
    );
    if let Some(name) = name {
        properties.insert("TableName".to_string(), json!(name));
    }
    json!({ "Type": "AWS::DynamoDB::Table", "Properties": Value::Object(properties) })
}

/// The whole projection as one template: the streamed source table, the sink table, the
/// execution role carrying `managed_policy` plus the one write the handler makes, the function
/// whose inline source is [`SINK_HANDLER`], and the mapping from the source table's stream
/// attribute to that function.
fn pipeline_template(source: Value, managed_policy: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a DynamoDB stream read by a Lambda function that writes a second table",
        "Resources": {
            SOURCE_TABLE: source,
            SINK_TABLE: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "AttributeDefinitions": [
                        { "AttributeName": SINK_KEY_ATTRIBUTE, "AttributeType": "S" }
                    ],
                    "KeySchema": [
                        { "AttributeName": SINK_KEY_ATTRIBUTE, "KeyType": "HASH" }
                    ],
                    "BillingMode": PAY_PER_REQUEST,
                },
            },
            FUNCTION_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "ManagedPolicyArns": [managed_policy],
                    "Policies": [{
                        "PolicyName": "write-the-sink-table",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "dynamodb:PutItem",
                                "Resource": { "Fn::GetAtt": [SINK_TABLE, "Arn"] },
                            }],
                        },
                    }],
                },
            },
            STREAM_FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": [FUNCTION_ROLE, "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {
                            "SINK_TABLE": { "Ref": SINK_TABLE },
                            "KEY_ATTRIBUTE": KEY_ATTRIBUTE,
                        },
                    },
                    "Code": { "ZipFile": SINK_HANDLER },
                },
            },
            MAPPING: {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "EventSourceArn": { "Fn::GetAtt": [SOURCE_TABLE, "StreamArn"] },
                    "FunctionName": { "Ref": STREAM_FUNCTION },
                    "StartingPosition": "TRIM_HORIZON",
                    "BatchSize": 5,
                },
            },
        },
        "Outputs": {
            "SourceTableName": { "Value": { "Ref": SOURCE_TABLE } },
            "SinkTableName": { "Value": { "Ref": SINK_TABLE } },
            "FunctionName": { "Value": { "Ref": STREAM_FUNCTION } },
            "EsmId": { "Value": { "Ref": MAPPING } },
            "SourceStreamArn": { "Value": { "Fn::GetAtt": [SOURCE_TABLE, "StreamArn"] } },
        },
    })
    .to_string()
}

/// Submit one stack and answer the status it settled in, with the `DescribeStacks` answer that
/// status was read from.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    giving_up_at: Instant,
) -> (String, String) {
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
    settled(router, stack, giving_up_at).await
}

/// Wait until the stack reports a TERMINAL status — CloudFormation's own statuses end
/// `_IN_PROGRESS` while an operation runs — and answer it with the answer it was read from.
///
/// The wait is on that CONDITION, looking again until it holds or `giving_up_at` passes, so
/// the completion a stack's resources owe is never named by a fixed sleep. The sibling's wait
/// is for ONE wanted status, which a refused stack would be waited out for.
///
/// `pub(crate)` because every suite that deploys a stack and then reads whatever status it
/// settled in needs exactly this, and a second copy of it drifts from this one.
pub(crate) async fn settled(
    router: &MultiServiceRouter,
    stack: &str,
    giving_up_at: Instant,
) -> (String, String) {
    loop {
        let reported = stack_status(router, stack).await;
        if !reported.0.ends_with("_IN_PROGRESS") || Instant::now() >= giving_up_at {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every `(logical id, reason)` the stack recorded a refused resource create under.
///
/// `pub(crate)` for the reason [`settled`] is: a suite that reads why a deployment did not
/// settle the way it expected needs this exact read.
pub(crate) async fn create_failures(
    router: &MultiServiceRouter,
    stack: &str,
) -> Vec<(String, String)> {
    let (status, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeStackEvents for {stack:?}, got {status} with body {events}"
    );
    members_of(&events)
        .into_iter()
        .filter(|member| block_of(member, "ResourceStatus") == Some(CREATE_FAILED))
        .map(|member| {
            (
                block_of(member, "LogicalResourceId")
                    .unwrap_or_default()
                    .to_string(),
                block_of(member, "ResourceStatusReason")
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Poll `GetEventSourceMapping` until Lambda reports the mapping in the state it READS in, and
/// answer the last configuration it reported.
///
/// The sibling's own wait stops as soon as the mapping leaves `Creating`, which is one state
/// earlier than the one the delivery below needs; this one waits for that state itself, on the
/// CONDITION, until it holds or `giving_up_at` passes.
async fn mapping_until_reading(
    router: &MultiServiceRouter,
    case: &str,
    uuid: &str,
    giving_up_at: Instant,
) -> Value {
    loop {
        let answered = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected GetEventSourceMapping to resolve the id the stack published \
             ({uuid:?}), got {} with body {:?}",
            answered.status,
            answered.text()
        );
        let read = answered.json();
        if read.get("State").and_then(Value::as_str) == Some(MAPPING_STATE_ENABLED)
            || Instant::now() >= giving_up_at
        {
            return read;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Write one item to the source table, as a client drives the projection.
async fn write_source(router: &MultiServiceRouter, case: &str, table: &str, key: &str, value: &str) {
    let (status, answered) = dynamodb(
        router,
        "PutItem",
        &json!({
            "TableName": table,
            "Item": {
                KEY_ATTRIBUTE: { "S": key },
                VALUE_ATTRIBUTE: { "S": value },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutItem on {table:?} under {key:?}, got {status} \
         with body {answered}"
    );
}

/// Poll `GetItem` on the sink table until the item under `id` is there, and answer it — or
/// `None` once `giving_up_at` passes with nothing written under that key.
async fn sink_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    id: &str,
    giving_up_at: Instant,
) -> Option<Value> {
    loop {
        let (status, answered) = dynamodb(
            router,
            "GetItem",
            &json!({
                "TableName": table,
                "Key": { SINK_KEY_ATTRIBUTE: { "S": id } },
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetItem on {table:?} under {id:?}, got {status} \
             with body {answered}"
        );
        if let Some(item) = answered.get("Item") {
            return Some(item.clone());
        }
        if Instant::now() >= giving_up_at {
            return None;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What the SINK table must hold once every write has been read off the stream: one item per
/// record the source produced, keyed by the record's key and its change name. A key written for
/// the first time is an insert; the same key written again is a change to it.
fn expected_sink_items() -> Vec<(String, Value)> {
    let mut seen: Vec<&str> = Vec::new();
    let mut wanted = Vec::new();
    for (key, _) in WRITES {
        let event_name = if seen.contains(key) { MODIFY } else { INSERT };
        seen.push(key);
        let id = format!("{key}#{event_name}");
        let item = json!({
            SINK_KEY_ATTRIBUTE: { "S": id.clone() },
            "source_key": { "S": *key },
            "event_name": { "S": event_name },
        });
        wanted.push((id, item));
    }
    wanted
}

/// The declared trigger hands its function the source's records, and the handler's own write
/// is in the declared sink table afterwards.
#[tokio::test]
async fn a_declared_trigger_drives_its_function_to_write_the_declared_sink_table() {
    let case = "a declared projection from a streamed table into a second declared table";
    let router = emulator();
    let stack = "xsvc-ddb-stream-sink-projection";
    let giving_up_at = Instant::now() + WAIT;
    let template = pipeline_template(source_table(None), DYNAMODB_EXECUTION_POLICY);

    let (settled_status, described) = deploy(&router, case, stack, &template, giving_up_at).await;
    let failures = if settled_status == CREATE_COMPLETE {
        Vec::new()
    } else {
        create_failures(&router, stack).await
    };
    assert_eq!(
        settled_status, CREATE_COMPLETE,
        "case {case:?}: expected the stack to settle {CREATE_COMPLETE}, got {described} with \
         refused resources {failures:?}"
    );

    let outputs = outputs_of(&described);
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {resources}"
    );
    let physical = physical_ids_of(&resources);
    for (output, logical) in [
        ("SourceTableName", SOURCE_TABLE),
        ("SinkTableName", SINK_TABLE),
        ("FunctionName", STREAM_FUNCTION),
        ("EsmId", MAPPING),
    ] {
        let created = physical.get(logical).cloned();
        assert!(
            created.is_some(),
            "case {case:?}: expected the stack to report a physical id for {logical}, got \
             {physical:?}"
        );
        assert_eq!(
            outputs.get(output).cloned(),
            created,
            "case {case:?}: expected the {output} output to be the physical id of {logical} \
             ({created:?}), got {outputs:?}"
        );
    }
    let source = outputs
        .get("SourceTableName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no SourceTableName output in {outputs:?}"));
    let sink = outputs
        .get("SinkTableName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no SinkTableName output in {outputs:?}"));
    let mapping_id = outputs
        .get("EsmId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no EsmId output in {outputs:?}"));

    // The source the mapping reads is the stream the table itself reports, which is also what
    // the stack published for the attribute the template read off that table.
    let (status, described_source) = described_table(&router, &source).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeTable for {source:?}, got {status} with \
         body {described_source}"
    );
    let minted = described_source
        .pointer("/Table/LatestStreamArn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        outputs.get("SourceStreamArn").map(String::as_str),
        Some(minted.as_str()),
        "case {case:?}: expected the StreamArn attribute to be the stream the table reports \
         ({minted:?}), got {outputs:?}"
    );

    // The mapping the stack published is one Lambda resolves by that id, reading that stream.
    let read = mapping_until_reading(&router, case, &mapping_id, giving_up_at).await;
    assert_eq!(
        read.get("State").and_then(Value::as_str),
        Some(MAPPING_STATE_ENABLED),
        "case {case:?}: expected the mapping to reach {MAPPING_STATE_ENABLED}, got {read}"
    );
    assert_eq!(
        read.get("EventSourceArn").and_then(Value::as_str),
        Some(minted.as_str()),
        "case {case:?}: expected the mapping to read {minted:?}, got {read}"
    );

    // The trigger really fires, and the handler's own call really wrote: each write to the
    // source turns up in the SINK table as the item the handler builds from the record.
    for (key, value) in WRITES {
        write_source(&router, case, &source, key, value).await;
    }
    for (id, wanted) in expected_sink_items() {
        let held = sink_item(&router, case, &sink, &id, giving_up_at).await;
        assert_eq!(
            held,
            Some(wanted.clone()),
            "case {case:?}: expected the sink table to hold {wanted} under {id:?}, got {held:?}"
        );
    }
}

/// The refusal half, as AWS Lambda refuses it: the only thing changed from the deploying
/// template is the managed policy the execution role carries — with the basic one the role
/// cannot make the poller's reads on the stream, which is what the mapping's create is decided
/// on. The source table is NAMED here so the stream that refusal names is computable from the
/// declaration rather than read back out of the answer under test.
#[tokio::test]
async fn a_declared_trigger_whose_role_cannot_read_the_stream_fails_with_its_reason() {
    let case = "an execution role carrying only the basic managed policy";
    let router = emulator();
    let stack = "xsvc-ddb-stream-sink-unreadable";
    let source = format!("{stack}-source");
    let giving_up_at = Instant::now() + WAIT;
    let template = pipeline_template(source_table(Some(&source)), BASIC_EXECUTION_POLICY);

    // The sentence Lambda refuses a mapping whose role cannot read the source with, as that
    // service words that error class: the stream is named from the declaration, and the actions
    // are the ones a DynamoDB stream's poller makes.
    let named =
        format!("Cannot access stream arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{source}/stream/");
    let advice = "Please ensure the role can perform the GetRecords, GetShardIterator, \
                  DescribeStream, and ListStreams Actions on your stream in IAM.";
    let reason = refusal_of(&router, case, stack, &template, MAPPING, giving_up_at).await;
    assert!(
        reason.contains(&named) && reason.contains(advice),
        "case {case:?}: expected {MAPPING}'s reason to carry {named:?} and {advice:?}, got \
         {reason:?}"
    );
}

/// Deploy a template whose `logical` resource the owning service refuses, and answer the reason
/// the stack recorded that resource's `CREATE_FAILED` event with — asserting on the way that
/// the stack settled `ROLLBACK_COMPLETE` and that the failure was reported against that
/// resource and no other.
async fn refusal_of(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    logical: &str,
    giving_up_at: Instant,
) -> String {
    let (settled_status, described) = deploy(router, case, stack, template, giving_up_at).await;
    assert_eq!(
        settled_status, ROLLBACK_COMPLETE,
        "case {case:?}: expected the stack to settle {ROLLBACK_COMPLETE}, got {described}"
    );
    let failures = create_failures(router, stack).await;
    let refused: Vec<&(String, String)> = failures
        .iter()
        .filter(|(failed, _)| failed == logical)
        .collect();
    assert!(
        !refused.is_empty(),
        "case {case:?}: expected a {CREATE_FAILED} event for {logical}, got {failures:?}"
    );
    assert!(
        failures.iter().all(|(failed, _)| failed == logical),
        "case {case:?}: expected {logical} to be the only resource reported {CREATE_FAILED}, got \
         {failures:?}"
    );
    refused[0].1.clone()
}
