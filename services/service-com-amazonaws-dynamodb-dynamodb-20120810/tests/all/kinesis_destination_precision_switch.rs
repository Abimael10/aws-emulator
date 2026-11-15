// SPDX-License-Identifier: CC0-1.0

//! Which UNIT a table's Kinesis streaming destination states a delivered record's creation
//! time in, across a configuration change of the destination.
//!
//! RULE: `ApproximateCreationDateTime` on every change record a table delivers to its
//! Kinesis streaming destination is stated in the unit the destination's
//! `ApproximateCreationDateTimePrecision` names at the instant the change was made — the
//! enum's `MILLISECOND` or `MICROSECOND`
//! (`smithy_query(query="shape",shape="ApproximateCreationDateTimePrecision")`), and
//! `MILLISECOND` for a destination no command has named one for — so an
//! `UpdateKinesisStreamingDestination` that names a different precision switches the unit of
//! the records delivered AFTER it and leaves the records delivered before it stated as they
//! were.
//!
//! The sibling suite `kinesis_destination_record_delivery` pins that rule for a destination
//! whose precision never changes, and for ONE direction of the change (`MILLISECOND` ->
//! `MICROSECOND`) over `PutItem`. The rows here differ along the axes that one does not:
//!
//! * the DIRECTION of the change — `MILLISECOND` -> `MICROSECOND` and `MICROSECOND` ->
//!   `MILLISECOND`, so neither unit is the one a switch can only move towards;
//! * the precision the destination STARTED under — named by the enable, and left unnamed so
//!   the service's own default is what the earlier records are stated in;
//! * a change that names the unit already in force for an unnamed precision, which moves no
//!   record's unit at all;
//! * the operation that makes the change delivered after the switch (`PutItem`,
//!   `UpdateItem`, `BatchWriteItem`, `TransactWriteItems`), because the unit is a property of
//!   the destination rather than of the write that fed it.
//!
//! Every command, every write and every record is driven over the wire (`POST /` with
//! `X-Amz-Target: <Service>.<OperationName>`) through the same `MultiServiceRouter` the
//! emulator's front doors use, with the DynamoDB and the Kinesis service mounted on ONE
//! shared engine; the records are read back through Kinesis's own
//! `GetShardIterator`/`GetRecords`, where a change-data-capture consumer reads them.

use serde_json::{json, Value};

use crate::contributor_insights::{create_table, settle};
use crate::emulator::{emulator, ensure_kinesis_stream, ok};
use crate::kinesis_destination_record_delivery::{
    assert_creation_time, delivered, enable, MICROSECOND, MILLISECOND,
};

/// The partition-key attribute of the tables this suite creates
/// (`crate::contributor_insights::create_table`).
const KEY: &str = "Part";

/// One row: where the destination's precision starts, what the configuration change names,
/// the unit each side of the change is stated in, and which operation makes the change
/// delivered after it.
struct Row {
    case: &'static str,
    /// The precision the enable names, or `None` where it names none.
    enabled_with: Option<&'static str>,
    /// The precision the configuration change names.
    updated_to: &'static str,
    /// The unit the record delivered BEFORE the change is stated in.
    unit_before: &'static str,
    /// The unit the record delivered AFTER the change is stated in.
    unit_after: &'static str,
    /// The operation the change delivered after the switch is made with.
    after_operation: &'static str,
}

/// The request body `operation` carries to store one fresh item of `table` under `key`.
///
/// Each is the same item-level change — one insert — made through a different write surface:
/// the singleton item API, the batch API and the transactional API.
fn write_request(operation: &str, table: &str, key: &str) -> Value {
    let item = json!({KEY: {"S": key}, "Value": {"S": "stated"}});
    match operation {
        "PutItem" => json!({"TableName": table, "Item": item}),
        "UpdateItem" => json!({
            "TableName": table,
            "Key": {KEY: {"S": key}},
            "UpdateExpression": "SET #value = :value",
            "ExpressionAttributeNames": {"#value": "Value"},
            "ExpressionAttributeValues": {":value": {"S": "stated"}},
        }),
        "BatchWriteItem" => json!({
            "RequestItems": {table: [{"PutRequest": {"Item": item}}]},
        }),
        "TransactWriteItems" => json!({
            "TransactItems": [{"Put": {"TableName": table, "Item": item}}],
        }),
        other => panic!("the suite drives no write called {other:?}"),
    }
}

/// Drive one row end to end and answer the records its destination's stream holds, oldest
/// first: one write under the precision the enable named, the configuration change, then one
/// write under the precision the change named.
async fn records_across_the_change(row: &Row) -> Vec<Value> {
    let router = emulator();
    let stream = "cdc";
    let stream_arn = ensure_kinesis_stream(&router, stream).await;
    let table = "Switching";
    create_table(&router, table, &[]).await;
    enable(&router, table, &stream_arn, row.enabled_with).await;
    // The registration is asynchronous, so the destination takes changes only once its own
    // span has fallen due — the table's create span falls due inside the same wait.
    settle().await;

    ok(&router, "PutItem", write_request("PutItem", table, "before")).await;

    let answered = ok(
        &router,
        "UpdateKinesisStreamingDestination",
        json!({
            "TableName": table,
            "StreamArn": stream_arn.clone(),
            "UpdateKinesisStreamingConfiguration": {
                "ApproximateCreationDateTimePrecision": row.updated_to,
            },
        }),
    )
    .await;
    assert_eq!(
        answered["UpdateKinesisStreamingConfiguration"]
            ["ApproximateCreationDateTimePrecision"],
        json!(row.updated_to),
        "case {:?}: the change reports the precision now in force, got {answered}",
        row.case
    );
    settle().await;

    // The destination REPORTS the precision now in force, read back off the record rather
    // than off the command that set it — so a row whose change names the unit already in
    // force carries its own signal too.
    let reported = ok(
        &router,
        "DescribeKinesisStreamingDestination",
        json!({"TableName": table}),
    )
    .await;
    let destination = reported["KinesisDataStreamDestinations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|destination| destination["StreamArn"] == json!(stream_arn))
        .unwrap_or(Value::Null);
    assert_eq!(
        destination["ApproximateCreationDateTimePrecision"],
        json!(row.updated_to),
        "case {:?}: the destination reports the precision in force, got {destination}",
        row.case
    );

    ok(
        &router,
        row.after_operation,
        write_request(row.after_operation, table, "after"),
    )
    .await;

    delivered(&router, row.case, stream).await
}

/// Assert the two records `row` delivered are the insert made before the configuration
/// change and the one made after it, each stated in that row's own unit.
fn assert_units_across_the_change(row: &Row, records: &[Value]) {
    let keys: Vec<Value> = records
        .iter()
        .map(|record| record["dynamodb"]["Keys"].clone())
        .collect();
    assert_eq!(
        keys,
        vec![
            json!({KEY: {"S": "before"}}),
            json!({KEY: {"S": "after"}}),
        ],
        "case {:?}: expected the change before the switch then the one after it, got \
         {records:?}",
        row.case
    );
    assert_creation_time(
        &format!("{}: the record before the change", row.case),
        &records[0],
        row.unit_before,
    );
    assert_creation_time(
        &format!("{}: the record after the change", row.case),
        &records[1],
        row.unit_after,
    );
}

/// The rule in EITHER direction: a destination that named its precision at the enable has the
/// unit of its later records switched to whichever of the two the change names, and the
/// records already delivered keep the unit they were stated in.
#[tokio::test]
async fn a_precision_change_switches_the_unit_in_either_direction() {
    let rows = [
        Row {
            case: "milliseconds, then changed to microseconds",
            enabled_with: Some(MILLISECOND),
            updated_to: MICROSECOND,
            unit_before: MILLISECOND,
            unit_after: MICROSECOND,
            after_operation: "BatchWriteItem",
        },
        Row {
            case: "microseconds, then changed to milliseconds",
            enabled_with: Some(MICROSECOND),
            updated_to: MILLISECOND,
            unit_before: MICROSECOND,
            unit_after: MILLISECOND,
            after_operation: "TransactWriteItems",
        },
    ];

    for row in &rows {
        let records = records_across_the_change(row).await;
        assert_eq!(
            records.len(),
            2,
            "case {:?}: both writes are delivered, got {records:?}",
            row.case
        );
        assert_units_across_the_change(row, &records);
    }
}

/// The same rule where the enable named NO precision: the records before the change are
/// stated in the unit a destination that named none delivers in, and the change moves the
/// later ones to the unit it names — which is no move at all when it names that same unit.
///
/// `ApproximateCreationDateTimePrecision` is an optional member and the model declares no
/// default for it
/// (`smithy_query(query="shape",shape="EnableKinesisStreamingConfiguration")`), so the unit
/// of a destination no command named one for is the one this store states
/// (`KinesisDestination::precision_in_force` in `src/state/table_settings.rs`): the rows
/// below pin it, and that it is the SAME unit across the no-move change, rather than
/// claiming AWS documents a default.
#[tokio::test]
async fn a_destination_that_named_no_precision_starts_in_the_default_unit() {
    let rows = [
        Row {
            case: "no precision named, then changed to microseconds",
            enabled_with: None,
            updated_to: MICROSECOND,
            unit_before: MILLISECOND,
            unit_after: MICROSECOND,
            after_operation: "UpdateItem",
        },
        Row {
            case: "no precision named, then named the unit already in force",
            enabled_with: None,
            updated_to: MILLISECOND,
            unit_before: MILLISECOND,
            unit_after: MILLISECOND,
            after_operation: "PutItem",
        },
    ];

    for row in &rows {
        let records = records_across_the_change(row).await;
        assert_eq!(
            records.len(),
            2,
            "case {:?}: both writes are delivered, got {records:?}",
            row.case
        );
        assert_units_across_the_change(row, &records);
    }
}
