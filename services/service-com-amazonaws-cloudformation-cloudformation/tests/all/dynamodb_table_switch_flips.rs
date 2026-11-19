// SPDX-License-Identifier: CC0-1.0

//! An `UpdateStack` that MOVES a switch of an `AWS::DynamoDB::Table` the stack already has —
//! server-side encryption, point-in-time recovery, Contributor Insights — applied to that
//! table in place.
//!
//! RULE: a property of a declared resource that the owning service's own in-place calls take
//! is APPLIED to the resource the stack already has when a deployment changes it, whichever
//! way the change goes and whether the revised declaration states the property or drops it;
//! the stack reaches `UPDATE_COMPLETE` keeping the resource's physical id, its own identity
//! and the data it holds, and the properties the declaration left alone keep the values the
//! create gave them. For `AWS::DynamoDB::Table` the calls are `UpdateTable` (whose own
//! `SSESpecification` is the type's property with the flag spelled `SSEEnabled`),
//! `UpdateContinuousBackups` and `UpdateContributorInsights` — so none of these switches can
//! make a stack un-updatable.
//!
//! The rows differ along what the rule generalises over: WHICH way the switch moves (off, and
//! on again), whether the revised declaration STATES the property as false or DROPS it
//! altogether, how many of the switches one deployment moves (all three at once, and one
//! alone), and the form the declaration takes (the flag alone, and the flag beside the
//! encryption type and the key identifier). Every row's table also carries a secondary index,
//! a table class and a TTL setting the deployment leaves ALONE, and an item written before the
//! update — which is what says the change was applied in place rather than by replacing the
//! table.
//!
//! The sibling suite `dynamodb_table_settings.rs` holds the same properties at CREATE time;
//! what this one adds is the deployment that MOVES one over a table that already exists.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_table_resources::{describe_table, dynamodb, failure_reasons};
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, router, xml_blocks, xml_text,
};

/// The logical id every row's template declares its table under.
const TABLE_LOGICAL_ID: &str = "Table";

/// How often a row looks again, and how many times. The wait is on the CONDITION — the stack
/// out of its in-progress status, the owning read reporting the setting settled — so nothing
/// here sleeps on the emulator's behalf, and the budget is far past the spans a table and the
/// switches of one owe several times over.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// The table class every row declares and no row changes, which is what says a property the
/// deployment left alone kept the value the create gave it.
const TABLE_CLASS: &str = "STANDARD_INFREQUENT_ACCESS";

/// The attribute every row's TTL setting expires items by, likewise untouched by the update.
const TTL_ATTRIBUTE: &str = "expiresAt";

/// The partition key of every row's table, and the index key beside it.
const PARTITION_KEY: &str = "pk";
const INDEX_KEY: &str = "gsi1pk";

/// The key of the item each row writes BEFORE the update and reads back after it.
const ITEM_KEY: &str = "written-before-the-update";

/// The declaration of one row's table apart from the switches the row moves: a key schema, a
/// `KEYS_ONLY` secondary index, a table class and a TTL setting.
fn base_properties(table: &str) -> Value {
    json!({
        "TableName": table,
        "BillingMode": "PAY_PER_REQUEST",
        "AttributeDefinitions": [
            {"AttributeName": PARTITION_KEY, "AttributeType": "S"},
            {"AttributeName": INDEX_KEY, "AttributeType": "S"},
        ],
        "KeySchema": [{"AttributeName": PARTITION_KEY, "KeyType": "HASH"}],
        "GlobalSecondaryIndexes": [{
            "IndexName": "by-tenant",
            "KeySchema": [{"AttributeName": INDEX_KEY, "KeyType": "HASH"}],
            "Projection": {"ProjectionType": "KEYS_ONLY"},
        }],
        "TableClass": TABLE_CLASS,
        "TimeToLiveSpecification": {"AttributeName": TTL_ATTRIBUTE, "Enabled": true},
    })
}

/// The template that declares one row's table with `switches` beside the properties above.
fn template(table: &str, switches: &Value) -> String {
    let mut properties = base_properties(table);
    let members = properties
        .as_object_mut()
        .expect("the declaration is an object");
    for (property, value) in switches
        .as_object()
        .expect("the row's switches are an object")
    {
        members.insert(property.clone(), value.clone());
    }
    json!({
        "Resources": {
            TABLE_LOGICAL_ID: {"Type": "AWS::DynamoDB::Table", "Properties": properties},
        },
    })
    .to_string()
}

/// What the owning reads must report once a row's update has completed.
struct Expected {
    /// Whether `DescribeTable` reports an `SSEDescription` at all: a table encrypted with the
    /// Amazon Web Services owned key carries none, which is the state a specification that
    /// does not enable encryption asks for.
    encrypted: bool,
    /// The `PointInTimeRecoveryStatus` `DescribeContinuousBackups` reports.
    recovery: &'static str,
    /// The `ContributorInsightsStatus` `DescribeContributorInsights` reports.
    insights: &'static str,
}

/// One row: the switches the create declares, the switches the update declares, and what the
/// owning reads must then report.
struct Row {
    case: &'static str,
    stack: &'static str,
    table: &'static str,
    created_with: Value,
    updated_to: Value,
    expected: Expected,
}

/// The three switches, each stated with the flag its own property spells.
fn switches(encrypted: bool, recovery: bool, insights: bool) -> Value {
    json!({
        "SSESpecification": {"SSEEnabled": encrypted},
        "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": recovery},
        "ContributorInsightsSpecification": {"Enabled": insights},
    })
}

/// The row the issue's own recording is one case of: all three switches declared on, then
/// stated off by the same template.
fn all_three_turned_off() -> Row {
    Row {
        case: "all three switches stated false by the revised template",
        stack: "ddb-flip-all-off",
        table: "flip-all-off-table",
        created_with: switches(true, true, true),
        updated_to: switches(false, false, false),
        expected: Expected {
            encrypted: false,
            recovery: "DISABLED",
            insights: "DISABLED",
        },
    }
}

/// The same move in the other direction: a table created with every switch off, turned on.
fn all_three_turned_on() -> Row {
    Row {
        case: "all three switches stated true by the revised template",
        stack: "ddb-flip-all-on",
        table: "flip-all-on-table",
        created_with: switches(false, false, false),
        updated_to: switches(true, true, true),
        expected: Expected {
            encrypted: true,
            recovery: "ENABLED",
            insights: "ENABLED",
        },
    }
}

/// The revised declaration that DROPS the three properties rather than stating them false:
/// the same state, asked for by omission.
fn all_three_dropped() -> Row {
    Row {
        case: "all three properties dropped by the revised template",
        stack: "ddb-flip-all-dropped",
        table: "flip-all-dropped-table",
        created_with: switches(true, true, true),
        updated_to: json!({}),
        expected: Expected {
            encrypted: false,
            recovery: "DISABLED",
            insights: "DISABLED",
        },
    }
}

/// One switch alone, moved while the other two stay where the create left them — and stated
/// in the fullest form the property takes: the flag, the encryption type and the key.
fn encryption_alone_turned_on() -> Row {
    Row {
        case: "encryption alone turned on, named with its type and key",
        stack: "ddb-flip-encryption-only",
        table: "flip-encryption-only-table",
        created_with: switches(false, true, true),
        updated_to: json!({
            "SSESpecification": {
                "SSEEnabled": true,
                "SSEType": "KMS",
                // The predefined alias DynamoDB encrypts a table with when a declaration
                // names no key of its own.
                "KMSMasterKeyId": "alias/aws/dynamodb",
            },
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
            "ContributorInsightsSpecification": {"Enabled": true},
        }),
        expected: Expected {
            encrypted: true,
            recovery: "ENABLED",
            insights: "ENABLED",
        },
    }
}

/// The `StackStatus` the stack reports right now, with the body it was read from.
async fn stack_status(case: &str, router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = describe_stack(router, stack).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeStacks to answer for {stack:?}, got {status} {body}"
    );
    (xml_text(&body, "StackStatus").unwrap_or_default(), body)
}

/// Poll `stack` until it reports a status no operation is still in flight in, and answer it
/// with the body it was read from.
async fn settled(case: &str, router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let mut last = stack_status(case, router, stack).await;
    for _ in 0..POLLS {
        if !last.0.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        last = stack_status(case, router, stack).await;
    }
    last
}

/// The physical id `DescribeStackResources` records for the declared table.
async fn physical_id(case: &str, router: &MultiServiceRouter, stack: &str) -> Option<String> {
    let (status, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeStackResources to answer, got {status} {body}"
    );
    xml_blocks(&body, "member")
        .into_iter()
        .find(|resource| {
            xml_text(resource, "LogicalResourceId").as_deref() == Some(TABLE_LOGICAL_ID)
        })
        .and_then(|resource| xml_text(&resource, "PhysicalResourceId"))
}

/// The value one DynamoDB read reports at `pointer` for `table`, looked at again until it is
/// `wanted` or the budget runs out — the last value it reported either way.
///
/// Every one of these switches is settled by the owning service a span after the call that
/// moved it, so the status a row expects is owed from the instant that span falls due and no
/// fixed sleep names it.
async fn reported(
    router: &MultiServiceRouter,
    operation: &str,
    table: &str,
    pointer: &str,
    wanted: Option<&Value>,
) -> Option<Value> {
    let mut last = None;
    for _ in 0..POLLS {
        let (_, answered) = dynamodb(router, operation, &json!({"TableName": table})).await;
        last = answered.pointer(pointer).cloned();
        if last.as_ref() == wanted {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// The `Table` description DynamoDB answers for `table`, which every row reads twice.
async fn table_description(case: &str, router: &MultiServiceRouter, table: &str) -> Value {
    describe_table(router, table).await.unwrap_or_else(|| {
        panic!("case {case:?}: expected DynamoDB to hold the table {table:?}, got no table at all")
    })
}

/// Deploy one row's table, write an item into it, and answer the table's own identity and the
/// physical id the stack recorded for it.
async fn created(router: &MultiServiceRouter, row: &Row) -> (Value, Option<String>) {
    let case = row.case;
    let body = template(row.table, &row.created_with);
    let (status, answered) = create_stack(router, row.stack, &body, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status} {answered}"
    );
    let (reported_status, described) = settled(case, router, row.stack).await;
    assert_eq!(
        reported_status, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reported_status:?} because {:?} \
         in {described}",
        failure_reasons(router, row.stack).await
    );

    let description = table_description(case, router, row.table).await;
    let identity = description
        .get("TableId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the table to report a TableId"));

    let item = json!({
        PARTITION_KEY: {"S": ITEM_KEY},
        INDEX_KEY: {"S": "tenant-1"},
        "note": {"S": case},
    });
    let (status, written) = dynamodb(
        router,
        "PutItem",
        &json!({"TableName": row.table, "Item": item}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the write before the update to be served, got {status} {written}"
    );

    (identity, physical_id(case, router, row.stack).await)
}

/// The deployment under test: the revised template, asserted to be ACCEPTED and to complete.
async fn updated(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let revised = template(row.table, &row.updated_to);
    let (status, answered) = call(
        router,
        "UpdateStack",
        &[
            member("StackName", row.stack),
            member("TemplateBody", &revised),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateStack to be accepted, got {status} {answered}"
    );
    let (reported_status, described) = settled(case, router, row.stack).await;
    assert_eq!(
        reported_status, "UPDATE_COMPLETE",
        "case {case:?}: moving the switches must update the stack in place, got \
         {reported_status:?} because {:?} in {described}",
        failure_reasons(router, row.stack).await
    );
}

/// What the owning reads report once the update has completed: the switches the row moved, the
/// properties it left alone, the table's own identity and the item written before it.
async fn reads_back(
    router: &MultiServiceRouter,
    row: &Row,
    identity: &Value,
    recorded: Option<String>,
) {
    let case = row.case;

    // Server-side encryption, as `DescribeTable` reports it: a table encrypted with the
    // Amazon Web Services owned key carries NO `SSEDescription` at all.
    let wanted_status = json!("ENABLED");
    let wanted = row.expected.encrypted.then_some(&wanted_status);
    let status = reported(
        router,
        "DescribeTable",
        row.table,
        "/Table/SSEDescription/Status",
        wanted,
    )
    .await;
    assert_eq!(
        status.as_ref(),
        wanted,
        "case {case:?}: expected DescribeTable to report {wanted:?} for the table's \
         encryption, got {status:?}"
    );

    // Point-in-time recovery and Contributor Insights, each through its own read.
    for (operation, pointer, expected) in [
        (
            "DescribeContinuousBackups",
            "/ContinuousBackupsDescription/PointInTimeRecoveryDescription/\
             PointInTimeRecoveryStatus",
            row.expected.recovery,
        ),
        (
            "DescribeContributorInsights",
            "/ContributorInsightsStatus",
            row.expected.insights,
        ),
    ] {
        let wanted = json!(expected);
        let answered = reported(router, operation, row.table, pointer, Some(&wanted)).await;
        assert_eq!(
            answered.as_ref(),
            Some(&wanted),
            "case {case:?}: expected {operation} to report {expected:?} at {pointer}, got \
             {answered:?}"
        );
    }

    // The properties the deployment left ALONE keep the values the create gave them, and the
    // table is the same table: its own identity is unchanged, so nothing was replaced.
    let description = table_description(case, router, row.table).await;
    assert_eq!(
        description.pointer("/TableClassSummary/TableClass"),
        Some(&json!(TABLE_CLASS)),
        "case {case:?}: expected the untouched table class to stay {TABLE_CLASS}, got \
         {description}"
    );
    assert_eq!(
        description.get("TableId"),
        Some(identity),
        "case {case:?}: expected the table's own identity to be unchanged at {identity}, got \
         {description}"
    );
    let now = physical_id(case, router, row.stack).await;
    assert_eq!(
        now, recorded,
        "case {case:?}: expected the resource to keep the physical id {recorded:?}, got {now:?}"
    );

    let ttl = reported(
        router,
        "DescribeTimeToLive",
        row.table,
        "/TimeToLiveDescription/AttributeName",
        Some(&json!(TTL_ATTRIBUTE)),
    )
    .await;
    assert_eq!(
        ttl.as_ref(),
        Some(&json!(TTL_ATTRIBUTE)),
        "case {case:?}: expected the untouched TTL setting to stay on {TTL_ATTRIBUTE}, got \
         {ttl:?}"
    );

    // And the item written BEFORE the update is still there: the switches were moved on the
    // table the stack already had.
    let (status, read) = dynamodb(
        router,
        "GetItem",
        &json!({
            "TableName": row.table,
            "Key": {PARTITION_KEY: {"S": ITEM_KEY}},
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the read after the update to be served, got {status} {read}"
    );
    assert_eq!(
        read.pointer("/Item/note"),
        Some(&json!({"S": case})),
        "case {case:?}: expected the item written before the update to survive it, got {read}"
    );
}

/// One row, end to end. Each phase is awaited through its own heap-allocated future, so the
/// test's own frame stays small whatever the request paths beneath it cost.
async fn a_moved_switch_is_applied_in_place(row: &Row) {
    let router = router();
    let (identity, recorded) = Box::pin(created(&router, row)).await;
    Box::pin(updated(&router, row)).await;
    Box::pin(reads_back(&router, row, &identity, recorded)).await;
}

#[tokio::test]
async fn every_switch_stated_false_is_applied_to_the_table_the_stack_has() {
    Box::pin(a_moved_switch_is_applied_in_place(&all_three_turned_off())).await;
}

#[tokio::test]
async fn every_switch_stated_true_is_applied_to_the_table_the_stack_has() {
    Box::pin(a_moved_switch_is_applied_in_place(&all_three_turned_on())).await;
}

#[tokio::test]
async fn a_dropped_switch_property_asks_for_the_setting_off() {
    Box::pin(a_moved_switch_is_applied_in_place(&all_three_dropped())).await;
}

#[tokio::test]
async fn one_switch_moved_alone_leaves_the_others_where_the_create_left_them() {
    Box::pin(a_moved_switch_is_applied_in_place(&encryption_alone_turned_on())).await;
}
