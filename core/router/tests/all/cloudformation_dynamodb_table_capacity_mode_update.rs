// SPDX-License-Identifier: CC0-1.0

//! An `UpdateStack` that moves an `AWS::DynamoDB::Table` between the read/write capacity
//! modes is applied IN PLACE, to the table the stack already has.
//!
//! The rule under test: **a changed declaration is applied through the owning service's own
//! update call, computed from the configuration the declaration RESOLVES to — so a property
//! the declaration drops because the configuration it asks for has nowhere to put it is part
//! of that one call rather than a change with no in-place path.** For a table the two such
//! properties are the capacity figures, each belonging to one mode: "if you set BillingMode as
//! `PROVISIONED`, you must specify this property. If you set BillingMode as `PAY_PER_REQUEST`,
//! you cannot specify this property"
//! (`smithy_query(query="doc",shape="CreateTableInput",member="ProvisionedThroughput")`), while
//! the on-demand ceiling "sets the maximum number of read and write units for the specified
//! on-demand table" (`smithy_query(query="doc",shape="OnDemandThroughput")`). `UpdateTable`
//! carries the mode and both figures on one input
//! (`smithy_query(query="operation",operation="UpdateTable")`), so one call expresses the whole
//! move, and a declaration that states no mode at all resolves to the provisioned one.
//!
//! Two crates have to agree for that to hold (CloudFormation computes and makes the call;
//! DynamoDB decides the table's capacity mode and keeps its data), so the suite lives here
//! rather than in either one — beside `cloudformation_dynamodb_table_in_place_update.rs`,
//! which holds the same rule for the capacity FIGURES and whose callers this suite reuses.
//!
//! What this pins is what an infrastructure-as-code client observes: the stack reaches
//! `UPDATE_COMPLETE`, the table keeps the physical id and the ARN its create published, the
//! mode DynamoDB reports is the declared one, an item written before the deployment is still
//! readable and a write after it reads back.
//!
//! The rows differ along what the rule generalises over: the DIRECTION of the move, whether
//! the mode is stated or left to the default a declaration resolves to, which capacity figure
//! the move drops and which it adds, the capacity values, and whether the template NAMES the
//! table or leaves CloudFormation to name it (so the physical id the update must keep is a
//! generated one too). The last row moves nothing at all — it only drops the mode member for
//! the mode already in force — which is the same configuration and must stay the same table.
//! Each row is a test of its own over an engine of its own, so they run beside one another.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery CloudFormation caller, the poll that waits for a stack to FIRST report a
// status, the DynamoDB caller and the XML element readers are each written ONCE in this test
// crate: a suite that copies one drifts from the others.
use crate::cloudformation_apigatewayv2_resources::cloudformation;
use crate::cloudformation_dynamodb_table_in_place_update::{
    described_table, dynamodb, published_table_name,
};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::elements;

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The logical id every row declares its table under.
const TABLE_LOGICAL_ID: &str = "Table";

/// The partition key every row's table is declared with.
const KEY_ATTRIBUTE: &str = "pk";

/// The two values `BillingMode` names
/// (`smithy_query(query="shape",shape="BillingMode")`); the provisioned one is also the mode a
/// declaration that states none resolves to.
const PROVISIONED: &str = "PROVISIONED";
const PAY_PER_REQUEST: &str = "PAY_PER_REQUEST";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// A provisioned capacity pair, as a declaration carries it.
///
/// The pair ALONE, rather than the sibling suite's `provisioned`, which states the capacity
/// mode beside it: the rows below turn on whether the mode is stated at all, so each one
/// states its own mode (or states none) and takes only the figures from here.
fn pair(read: i64, write: i64) -> Value {
    json!({"ReadCapacityUnits": read, "WriteCapacityUnits": write})
}

/// One row: a table deployed with one set of capacity properties and then re-declared with
/// another, and what the owning service must report for it afterwards.
struct Row {
    case: &'static str,
    /// The stack this row deploys. Distinct per row: each row is its own test, run in
    /// parallel with the others on an engine of its own.
    stack: &'static str,
    /// The table name both templates declare, or `None` where CloudFormation names it.
    declared_name: Option<&'static str>,
    /// The capacity properties the create declares beside the key schema...
    create: Value,
    /// ...and the ones the update declares in their place.
    update: Value,
    /// The `BillingModeSummary.BillingMode` the table must report once the update completes,
    /// or `None` for a row whose table has never been on-demand and therefore carries no
    /// summary at all ([`reported_capacity_mode`]).
    summary: Option<&'static str>,
    /// JSON pointers into the `Table` the read answers, each with the value the updated
    /// declaration puts there — one per capacity figure the row's new mode takes.
    in_force: Vec<(&'static str, Value)>,
}

/// The case name of each row below, which is also the name the row's own test takes it from
/// the table by — so the table stays the one list of what the rule is read over, while each
/// row runs as a test of its own.
const TO_ON_DEMAND: &str = "provisioned to on-demand: the pair the new mode cannot hold is dropped";
const TO_PROVISIONED: &str = "on-demand to provisioned: the new mode's own pair travels with it";
const DEFAULT_MODE: &str = "the mode never stated: a provisioned table taken on-demand, unnamed";
const MODE_DROPPED: &str =
    "the mode member dropped: the declaration resolves to the provisioned mode";
const CEILING_DROPPED: &str = "the on-demand ceiling dropped with the mode that has one";
const CEILING_ADDED: &str = "a ceiling added as the table goes on-demand";
const MODE_RESTATED: &str =
    "the mode member dropped for the mode already in force: the same table";

/// The row named `case`, which is how each test below names the one it drives.
fn row(case: &str) -> Row {
    rows()
        .into_iter()
        .find(|row| row.case == case)
        .unwrap_or_else(|| panic!("no row named {case:?} in this suite's table"))
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: TO_ON_DEMAND,
            stack: "ddb-capacity-mode-to-on-demand",
            declared_name: Some("cfn-ddb-mode-on-demand"),
            create: json!({"BillingMode": PROVISIONED, "ProvisionedThroughput": pair(5, 5)}),
            update: json!({"BillingMode": PAY_PER_REQUEST}),
            summary: Some(PAY_PER_REQUEST),
            in_force: Vec::new(),
        },
        Row {
            case: TO_PROVISIONED,
            stack: "ddb-capacity-mode-to-provisioned",
            declared_name: Some("cfn-ddb-mode-provisioned"),
            create: json!({"BillingMode": PAY_PER_REQUEST}),
            update: json!({"BillingMode": PROVISIONED, "ProvisionedThroughput": pair(2, 3)}),
            summary: Some(PROVISIONED),
            in_force: vec![
                ("/ProvisionedThroughput/ReadCapacityUnits", json!(2)),
                ("/ProvisionedThroughput/WriteCapacityUnits", json!(3)),
            ],
        },
        Row {
            case: DEFAULT_MODE,
            stack: "ddb-capacity-mode-default",
            declared_name: None,
            create: json!({"ProvisionedThroughput": pair(1, 1)}),
            update: json!({"BillingMode": PAY_PER_REQUEST}),
            summary: Some(PAY_PER_REQUEST),
            in_force: Vec::new(),
        },
        Row {
            case: MODE_DROPPED,
            stack: "ddb-capacity-mode-dropped",
            declared_name: Some("cfn-ddb-mode-dropped"),
            create: json!({"BillingMode": PAY_PER_REQUEST}),
            update: json!({"ProvisionedThroughput": pair(4, 6)}),
            summary: Some(PROVISIONED),
            in_force: vec![
                ("/ProvisionedThroughput/ReadCapacityUnits", json!(4)),
                ("/ProvisionedThroughput/WriteCapacityUnits", json!(6)),
            ],
        },
        Row {
            case: CEILING_DROPPED,
            stack: "ddb-capacity-mode-ceiling-dropped",
            declared_name: Some("cfn-ddb-mode-ceiling-dropped"),
            create: json!({"BillingMode": PAY_PER_REQUEST,
                           "OnDemandThroughput": {"MaxReadRequestUnits": 10,
                                                  "MaxWriteRequestUnits": 11}}),
            update: json!({"BillingMode": PROVISIONED, "ProvisionedThroughput": pair(7, 8)}),
            summary: Some(PROVISIONED),
            in_force: vec![
                ("/ProvisionedThroughput/ReadCapacityUnits", json!(7)),
                ("/ProvisionedThroughput/WriteCapacityUnits", json!(8)),
            ],
        },
        Row {
            case: CEILING_ADDED,
            stack: "ddb-capacity-mode-ceiling-added",
            declared_name: Some("cfn-ddb-mode-ceiling-added"),
            create: json!({"BillingMode": PROVISIONED, "ProvisionedThroughput": pair(9, 9)}),
            update: json!({"BillingMode": PAY_PER_REQUEST,
                           "OnDemandThroughput": {"MaxReadRequestUnits": 12}}),
            summary: Some(PAY_PER_REQUEST),
            in_force: vec![("/OnDemandThroughput/MaxReadRequestUnits", json!(12))],
        },
        Row {
            case: MODE_RESTATED,
            stack: "ddb-capacity-mode-restated",
            declared_name: Some("cfn-ddb-mode-restated"),
            create: json!({"BillingMode": PROVISIONED, "ProvisionedThroughput": pair(3, 3)}),
            update: json!({"ProvisionedThroughput": pair(3, 3)}),
            // Provisioned at the create and provisioned after the move, so this table has
            // never been on-demand and carries no summary at all.
            summary: None,
            in_force: vec![
                ("/ProvisionedThroughput/ReadCapacityUnits", json!(3)),
                ("/ProvisionedThroughput/WriteCapacityUnits", json!(3)),
            ],
        },
    ]
}

/// The template one deployment of a row submits: its table, with the capacity properties the
/// deployment declares beside the key schema every row shares.
fn template(row: &Row, capacity: &Value) -> String {
    let mut declared = json!({
        "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}],
    });
    let object = declared
        .as_object_mut()
        .expect("the declaration is an object");
    if let Some(name) = row.declared_name {
        object.insert("TableName".to_string(), json!(name));
    }
    for (member, value) in capacity
        .as_object()
        .expect("a row's capacity properties are an object")
    {
        object.insert(member.clone(), value.clone());
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            TABLE_LOGICAL_ID: {"Type": "AWS::DynamoDB::Table", "Properties": declared},
        }
    })
    .to_string()
}

/// The capacity mode a `DescribeTable` answer reports for the table, and whether it carried
/// a `BillingModeSummary` to report it in.
///
/// The summary is carried exactly by a table that has been on-demand at some point — "you may
/// need to switch to on-demand mode at least once in order to return a BillingModeSummary
/// response" (`smithy_query(query="shape",shape="BillingModeSummary")`) — so a table that has
/// only ever been provisioned carries none, and its absence is itself the answer for such a
/// row rather than a value to fall back on.
fn reported_capacity_mode(table: &Value) -> Option<String> {
    Some(
        table
            .pointer("/BillingModeSummary/BillingMode")?
            .as_str()?
            .to_string(),
    )
}

/// The `Table` DynamoDB reports for `name`, which it must hold at every point the stack
/// reports the resource created or updated.
async fn table_of(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, answered) = described_table(router, name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DynamoDB to hold the table {name:?}, got {status} with \
         {answered}"
    );
    answered["Table"].clone()
}

/// Write one item keyed by `key` to `name`, through DynamoDB's own wire.
async fn put_item(router: &MultiServiceRouter, case: &str, name: &str, key: &str) {
    let request = json!({
        "TableName": name,
        "Item": {KEY_ATTRIBUTE: {"S": key}, "note": {"S": key}},
    });
    let (status, answered) = dynamodb(router, "PutItem", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutItem {key:?} on {name:?} to be served, got {status} with \
         {answered}"
    );
}

/// The item `name` holds under `key`, or `None` where it holds none.
async fn item_of(router: &MultiServiceRouter, case: &str, name: &str, key: &str) -> Option<Value> {
    let request = json!({
        "TableName": name,
        "Key": {KEY_ATTRIBUTE: {"S": key}},
        "ConsistentRead": true,
    });
    let (status, answered) = dynamodb(router, "GetItem", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetItem {key:?} on {name:?} to be served, got {status} with \
         {answered}"
    );
    answered.get("Item").cloned()
}

/// The reasons the stack recorded for its failed resources, for a case whose deployment did
/// not reach the status the rule owes it.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// One row: the update completes, the table is the SAME table — same physical id, same ARN,
/// the item written before the change still readable — and the capacity configuration DynamoDB
/// reports is the one the new declaration asked for.
///
/// Each row of the table is its own test below, so a loaded host runs the seven deployments
/// beside one another rather than stacking them into one test's wall time.
async fn a_capacity_mode_move_is_applied_in_place(row: Row) {
    let router = build_router();
    let case = row.case;
    let stack = row.stack;

    // ---- The create the update is deployed against. ----
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template(&row, &row.create)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let (stack_status, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        stack_status, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE} before the update, \
         got {described} with resource failures {:?}",
        failure_reasons(&router, stack).await
    );
    let name = published_table_name(&router, case, stack).await;
    if let Some(declared) = row.declared_name {
        assert_eq!(
            name, declared,
            "case {case:?}: expected the create to publish the declared table name \
             {declared:?}, got {name:?}"
        );
    }
    let created = table_of(&router, case, &name).await;
    let arn = created["TableArn"].clone();
    assert!(
        arn.is_string(),
        "case {case:?}: expected the created table to report a TableArn, got {created}"
    );

    // An item written before the change: the move is data-preserving, so this item is
    // readable after it.
    put_item(&router, case, &name, "written-before").await;

    // ---- The capacity-mode move. ----
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template(&row, &row.update)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let (stack_status, described) = wait_for_stack_status(&router, stack, UPDATE_COMPLETE).await;
    assert_eq!(
        stack_status, UPDATE_COMPLETE,
        "case {case:?}: expected the capacity-mode move to be applied in place and the \
         stack to reach {UPDATE_COMPLETE}, got {described} with resource failures {:?}",
        failure_reasons(&router, stack).await
    );

    // ---- The table the completed update published. ----
    let updated_name = published_table_name(&router, case, stack).await;
    assert_eq!(
        updated_name, name,
        "case {case:?}: an in-place move keeps the table's identity, expected {name:?}, got \
         {updated_name:?}"
    );
    let table = table_of(&router, case, &updated_name).await;
    assert_eq!(
        table["TableArn"], arn,
        "case {case:?}: expected the table to keep the ARN {arn}, got {table}"
    );
    let mode = reported_capacity_mode(&table);
    assert_eq!(
        mode.as_deref(),
        row.summary,
        "case {case:?}: expected DynamoDB to report the capacity mode {:?} after the move, \
         got {mode:?} in {table}",
        row.summary
    );
    for (pointer, expected) in &row.in_force {
        assert_eq!(
            table.pointer(pointer),
            Some(expected),
            "case {case:?}: expected the table to report {pointer} as {expected}, got {table}"
        );
    }
    assert_eq!(
        table["KeySchema"],
        json!([{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}]),
        "case {case:?}: expected the key schema to survive the move, got {table}"
    );

    // The data survived the move, and the table takes writes after it.
    let kept = item_of(&router, case, &updated_name, "written-before").await;
    assert_eq!(
        kept,
        Some(json!({
            KEY_ATTRIBUTE: {"S": "written-before"},
            "note": {"S": "written-before"},
        })),
        "case {case:?}: expected the item written before the move to survive it, got {kept:?}"
    );
    put_item(&router, case, &updated_name, "written-after").await;
    let written = item_of(&router, case, &updated_name, "written-after").await;
    assert_eq!(
        written,
        Some(json!({
            KEY_ATTRIBUTE: {"S": "written-after"},
            "note": {"S": "written-after"},
        })),
        "case {case:?}: expected a write after the move to read back, got {written:?}"
    );
}

#[tokio::test]
async fn a_provisioned_table_is_taken_on_demand() {
    a_capacity_mode_move_is_applied_in_place(row(TO_ON_DEMAND)).await;
}

#[tokio::test]
async fn an_on_demand_table_is_taken_provisioned() {
    a_capacity_mode_move_is_applied_in_place(row(TO_PROVISIONED)).await;
}

#[tokio::test]
async fn a_table_declared_with_no_mode_is_taken_on_demand() {
    a_capacity_mode_move_is_applied_in_place(row(DEFAULT_MODE)).await;
}

#[tokio::test]
async fn dropping_the_mode_member_asks_for_the_provisioned_mode() {
    a_capacity_mode_move_is_applied_in_place(row(MODE_DROPPED)).await;
}

#[tokio::test]
async fn the_on_demand_ceiling_is_dropped_with_its_mode() {
    a_capacity_mode_move_is_applied_in_place(row(CEILING_DROPPED)).await;
}

#[tokio::test]
async fn a_ceiling_is_added_with_the_on_demand_mode() {
    a_capacity_mode_move_is_applied_in_place(row(CEILING_ADDED)).await;
}

#[tokio::test]
async fn dropping_the_mode_member_for_the_mode_in_force_changes_nothing() {
    a_capacity_mode_move_is_applied_in_place(row(MODE_RESTATED)).await;
}
