// SPDX-License-Identifier: CC0-1.0

//! WHEN a stack operation starts each of its resources, read off the owning services.
//!
//! RULE: "a resource that depends on another is scheduled only once that one has finished" is
//! the whole of the ordering a stack operation follows, so the resources no declaration of
//! the template relates to one another are STARTED TOGETHER: a stack operation costs about
//! one coming-up window per LEVEL of its dependency relation, however many resources that
//! level declares — never one window per resource.
//!
//! Without it a client deploying a template of several independent resources waits a multiple
//! of what the deployment owes: each resource's coming-up window is waited out before the
//! next resource is even asked for, and `CreateStack` does not answer until the whole walk is
//! through, so the cost is paid inside the call.
//!
//! The two halves are read where each of them is observable:
//!
//! 1. **Started together.** The level is declared with a type whose owning service really
//!    reports a coming-up window — `AWS::DynamoDB::Table`, whose `CreateTable` "immediately
//!    returns a response with a TableStatus of CREATING" — and that service records WHEN it
//!    was asked for each table (`TableDescription$CreationDateTime`). Every table of the
//!    level therefore reports a creation instant inside ONE window of the first of them,
//!    which is what "started together" means and what "one window per resource" cannot
//!    produce.
//! 2. **And only once what a resource reads has finished.** The mapping declared over a
//!    table's stream is created with the value that table publishes once it is up, and the
//!    table is `ACTIVE` by the time the stack reports the mapping created — so the walk still
//!    waits for a resource's dependency before it asks for the resource.
//!
//! The rows differ along HOW MANY resources the independent level declares — one, two, three
//! — which is what the rule generalises over: one is the degenerate level, and the wider ones
//! are where one window per LEVEL and one window per RESOURCE differ.
//!
//! The level is declared beside a second level-0 resource with no window of its own
//! (`AWS::IAM::Role`), a level-1 resource reading only that role
//! (`AWS::Lambda::Function`) and a level-2 resource reading one of the windowed ones
//! (`AWS::Lambda::EventSourceMapping` over a table's stream), so the relation really has
//! levels to walk.
//!
//! Nothing here asserts on how long the TEST took, or on how fast the host runs: what the
//! first half reads is the instants the owning service itself recorded for the creates it was
//! asked for, and the span they are held inside is that service's own coming-up window
//! ([`TABLE_CREATION_WINDOW`]) — the one limit these cases are about. Every wait is on a
//! condition, looked at again until it holds or a generous deadline passes.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::state::STATUS_TRANSITION;
use smithy_server_runtime::MultiServiceRouter;

// Every wire caller comes from the suite that owns it rather than from a second copy here.
use crate::cfn_ddb_stream_esm_lambda_pipeline::{create_failures, settled};
use crate::cloudformation_apigatewayv2_resources::cloudformation;
use crate::cloudformation_dynamodb_table_in_place_update::described_table;
use crate::cloudformation_provisions_lambda_event_source_mappings::MAPPINGS_URI;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::router as emulator;
use crate::logs_subscription_filter_to_lambda::lambda;
use crate::query_wire::{block_of, members_of};

/// The `StackStatus` and `ResourceStatus` the rule is stated in
/// (`com.amazonaws.cloudformation#StackStatus`, `#ResourceStatus`).
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `TableStatus` Amazon DynamoDB reports a table it has finished creating in
/// ("After the table is created, DynamoDB sets the TableStatus to ACTIVE").
const ACTIVE: &str = "ACTIVE";

/// The span Amazon DynamoDB holds a new table `CREATING` for — ITS OWN coming-up window, read
/// from that service rather than restated here, so this suite is about one window however
/// that service sizes it.
const TABLE_CREATION_WINDOW: Duration = STATUS_TRANSITION;

/// How long one row waits, in all, for the stack to reach a terminal status. A generous
/// WALL-CLOCK deadline rather than a count of reads: all it is for is that the wait
/// TERMINATES, so a broken row still reports promptly instead of hanging.
const WAIT: Duration = Duration::from_secs(300);

/// The logical ids the three levels are declared under.
const ROLE_ID: &str = "HandlerRole";
const FUNCTION_ID: &str = "Handler";
const MAPPING_ID: &str = "FirstMapping";

/// AWS's own published execution-role policy for a function that reads a DynamoDB stream.
const DYNAMODB_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaDynamoDBExecutionRole";

/// The runtime the declared function asks for, and the package it carries.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = r#"
def handler(event, context):
    return {"records": len(event.get("Records", []))}
"#;

/// One row: how many resources the stack's INDEPENDENT level declares.
struct Row {
    case: &'static str,
    stack: &'static str,
    tables: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one resource on the independent level",
            stack: "xsvc-levels-single",
            tables: 1,
        },
        Row {
            case: "two resources on the independent level",
            stack: "xsvc-levels-pair",
            tables: 2,
        },
        Row {
            case: "three resources on the independent level",
            stack: "xsvc-levels-triple",
            tables: 3,
        },
    ]
}

impl Row {
    /// The logical id the `index`-th table of the independent level is declared under.
    fn table_id(index: usize) -> String {
        format!("Table{index}")
    }

    /// Every logical id of the independent level, in declaration order.
    fn table_ids(&self) -> Vec<String> {
        (0..self.tables).map(Row::table_id).collect()
    }

    /// The name each declared table is created under, which the declaration STATES — so the
    /// reads below address the tables by a name computed from the template rather than one
    /// read back out of the answer under test.
    fn table_names(&self) -> Vec<String> {
        self.table_ids()
            .iter()
            .map(|id| format!("{}-{id}", self.stack))
            .collect()
    }
}

/// The estate as one template: `tables` streamed tables that read nothing of one another, the
/// execution role beside them, the function reading only that role, and one mapping reading
/// the FIRST table's stream.
fn template(row: &Row) -> String {
    let mut resources = Map::new();
    for (id, name) in row.table_ids().iter().zip(row.table_names()) {
        resources.insert(
            id.clone(),
            json!({
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": name,
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "BillingMode": "PAY_PER_REQUEST",
                    "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
                },
            }),
        );
    }
    resources.insert(
        ROLE_ID.to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {"Service": "lambda.amazonaws.com"},
                        "Action": "sts:AssumeRole",
                    }],
                },
                "ManagedPolicyArns": [DYNAMODB_EXECUTION_POLICY],
            },
        }),
    );
    resources.insert(
        FUNCTION_ID.to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": RUNTIME,
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": [ROLE_ID, "Arn"]},
                "Timeout": 120,
                "Code": {"ZipFile": HANDLER},
            },
        }),
    );
    resources.insert(
        MAPPING_ID.to_string(),
        json!({
            "Type": "AWS::Lambda::EventSourceMapping",
            "Properties": {
                "EventSourceArn": {"Fn::GetAtt": [Row::table_id(0), "StreamArn"]},
                "FunctionName": {"Ref": FUNCTION_ID},
                "StartingPosition": "LATEST",
                "BatchSize": 1,
            },
        }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "one level of independent resources, and the levels behind it",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Submit one row's stack and wait for it to settle, asserting it settled `CREATE_COMPLETE`.
async fn deploy(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let giving_up_at = Instant::now() + WAIT;
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &template(row)),
            // The template declares an IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    // The wait is on the CONDITION — a status that is no longer an operation in flight — and
    // looks again until it holds or the deadline passes.
    let (reached, described) = settled(router, row.stack, giving_up_at).await;
    assert_eq!(
        reached,
        CREATE_COMPLETE,
        "case {case:?}: expected the stack to settle {CREATE_COMPLETE}, got {described} with \
         resource failures {:?}",
        create_failures(router, row.stack).await
    );
}

/// What Amazon DynamoDB reports about one of the stack's tables: when it was asked to create
/// it, the status it is in now and the stream it publishes.
async fn reported_table(router: &MultiServiceRouter, case: &str, table: &str) -> Value {
    let (status, described) = described_table(router, table).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeTable for {table:?}, got {status} with \
         body {described}"
    );
    described
        .get("Table")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no Table in {described}"))
}

/// The instant Amazon DynamoDB recorded for a table's create, in seconds
/// (`TableDescription$CreationDateTime`).
fn created_at(case: &str, table: &str, reported: &Value) -> f64 {
    reported
        .get("CreationDateTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| {
            panic!("case {case:?}: no CreationDateTime for {table:?} in {reported}")
        })
}

/// RULE, first half: the resources of the stack's INDEPENDENT level were started together —
/// every one of them reports a creation instant inside ONE of its owning service's coming-up
/// windows, so the level costs one window and not one per resource.
#[tokio::test]
async fn the_independent_resources_of_one_level_are_created_inside_one_window() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        deploy(&router, &row).await;

        let mut created: Vec<(String, f64)> = Vec::new();
        for table in row.table_names() {
            let reported = reported_table(&router, case, &table).await;
            created.push((table.clone(), created_at(case, &table, &reported)));
        }
        let first = created
            .iter()
            .map(|(_, at)| *at)
            .fold(f64::INFINITY, f64::min);
        let last = created
            .iter()
            .map(|(_, at)| *at)
            .fold(f64::NEG_INFINITY, f64::max);
        let window = TABLE_CREATION_WINDOW.as_secs_f64();
        assert!(
            last - first < window,
            "case {case:?}: expected every resource of the independent level to have been \
             asked for inside one {window}s coming-up window, got {:.3}s between the first \
             and the last of {created:?}",
            last - first
        );
    }
}

/// RULE, second half: a resource is asked for only once what its declaration READS has
/// finished — the mapping declared over the first table's stream is created with the value
/// that table publishes once it is up, and that table is `ACTIVE` by the time the stack
/// reports the whole estate created.
#[tokio::test]
async fn a_resource_is_created_only_once_what_it_reads_is_up() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        deploy(&router, &row).await;

        // Every declaration of the template is a resource the stack reports created.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let reported: Vec<(String, String)> = members_of(&resources)
            .into_iter()
            .filter_map(|member| {
                Some((
                    block_of(member, "LogicalResourceId")?.to_string(),
                    block_of(member, "ResourceStatus")?.to_string(),
                ))
            })
            .collect();
        let mut declared = row.table_ids();
        declared.push(ROLE_ID.to_string());
        declared.push(FUNCTION_ID.to_string());
        declared.push(MAPPING_ID.to_string());
        for logical_id in declared {
            let status = reported
                .iter()
                .find(|(reported_id, _)| *reported_id == logical_id)
                .map(|(_, status)| status.as_str());
            assert_eq!(
                status,
                Some(CREATE_COMPLETE),
                "case {case:?}: expected the stack to report {logical_id} {CREATE_COMPLETE}, \
                 got {reported:?}"
            );
        }

        // ...and the table the mapping READS is up, with the stream it publishes being the
        // one the mapping was created to poll.
        let table = row
            .table_names()
            .first()
            .cloned()
            .expect("every row declares at least one table");
        let read = reported_table(&router, case, &table).await;
        assert_eq!(
            read.get("TableStatus").and_then(Value::as_str),
            Some(ACTIVE),
            "case {case:?}: expected {table:?} to be {ACTIVE} once the stack reported the \
             estate created, got {read}"
        );
        let stream = read
            .get("LatestStreamArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no LatestStreamArn in {read}"));
        let answered = lambda(&router, "GET", MAPPINGS_URI, None).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from ListEventSourceMappings, got {} with body \
             {:?}",
            answered.status,
            answered.text()
        );
        let listed = answered.json();
        let mappings = listed
            .get("EventSourceMappings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            mappings
                .iter()
                .any(|mapping| mapping.get("EventSourceArn").and_then(Value::as_str)
                    == Some(stream)),
            "case {case:?}: expected a mapping polling {table:?}'s published stream \
             {stream:?}, got {mappings:?}"
        );
    }
}
