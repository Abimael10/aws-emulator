// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the rule EVERY surface that describes a table's capacity settings
//! follows: the on-demand maximum-request-units block is reported exactly where such a
//! maximum is SET, and carried by no description of a resource that has none.
//!
//! `OnDemandThroughput` is the maximum-request-units SETTING — "sets the maximum number of
//! read and write units for the specified on-demand table" (the shape's doc) — and -1 is
//! the value a REQUEST sends to take a maximum away: "to specify a maximum
//! `OnDemandThroughput` on your table, set the value of `MaxReadRequestUnits` as greater
//! than or equal to 1. To remove the maximum `OnDemandThroughput` that is currently set on
//! your table, set the value of `MaxReadRequestUnits` to -1"
//! (`OnDemandThroughput$MaxReadRequestUnits`, which `MaxWriteRequestUnits` repeats). So a
//! resource nobody capped has no such setting to report and its description omits the block
//! rather than materialising it out of the removal value, while a resource that IS capped
//! reports the pair, with the uncapped dimension reading as the -1 that means "no maximum"
//! for it.
//!
//! The rows of the first test are the SURFACES that describe one table — the create's own
//! answer, the reader, an update's answer, the delete's answer, and the record a backup of
//! that table keeps of it (`SourceTableDetails$OnDemandThroughput`,
//! `smithy_query(query="shape", shape="SourceTableDetails")`) — so one rule is asserted
//! across all of them instead of one surface at a time. The rows of the second test are the
//! ways a maximum comes to be, or not be, in force at the instant a backup is taken, and
//! each row then moves the table's maximum AFTERWARDS: what the backup reports is what the
//! table carried when it was taken.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use, and every wait polls for the CONDITION it waits on.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{describe_when_active, emulator, ok, ACTIVE_DEADLINE, POLL_INTERVAL};

/// The on-demand maximum `described` reports, or `Value::Null` when it carries no such
/// member at all — which is how a client that reads a description back into its next
/// request sees it.
fn block(described: &Value) -> Value {
    described
        .get("OnDemandThroughput")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Create one table in `billing_mode`, with `maximum` as its `OnDemandThroughput` when the
/// row names one, and answer the create's own `TableDescription`.
///
/// A provisioned table is created with the capacity pair that mode requires; an on-demand
/// one names none.
async fn create_table(
    router: &MultiServiceRouter,
    name: &str,
    billing_mode: &str,
    maximum: Option<Value>,
) -> Value {
    let mut request = json!({
        "TableName": name,
        "BillingMode": billing_mode,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
    });
    if billing_mode == "PROVISIONED" {
        request["ProvisionedThroughput"] = json!({"ReadCapacityUnits": 5, "WriteCapacityUnits": 5});
    }
    if let Some(maximum) = maximum {
        request["OnDemandThroughput"] = maximum;
    }
    let created = ok(router, "CreateTable", request).await;
    created["TableDescription"].clone()
}

/// Send `members` as an `UpdateTable` on `table`, wait for the table to settle again, and
/// answer the update's own `TableDescription`.
async fn update_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    members: Value,
) -> Value {
    let mut request = json!({"TableName": table});
    for (member, value) in members.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    let updated = ok(router, "UpdateTable", request).await;
    describe_when_active(router, case, table).await;
    updated["TableDescription"].clone()
}

/// Take a backup of `table` and answer the ARN the catalogue gave it.
async fn create_backup(router: &MultiServiceRouter, table: &str, backup_name: &str) -> String {
    let created = ok(
        router,
        "CreateBackup",
        json!({"TableName": table, "BackupName": backup_name}),
    )
    .await;
    created["BackupDetails"]["BackupArn"]
        .as_str()
        .unwrap_or_else(|| panic!("an accepted CreateBackup reports the backup's ARN, got {created}"))
        .to_string()
}

/// The `BackupDescription` `DescribeBackup` reports for `backup_arn`.
async fn describe_backup(router: &MultiServiceRouter, backup_arn: &str) -> Value {
    let described = ok(router, "DescribeBackup", json!({"BackupArn": backup_arn})).await;
    described["BackupDescription"].clone()
}

/// Poll `DescribeBackup` until the backup reports `AVAILABLE`, which is when the catalogue
/// lets it be removed ("the backup is either being created, deleted or restored to a
/// table" — `BackupInUseException`).
///
/// The waiter loop a client runs after a `CreateBackup`, bounded by the shared deadline: a
/// backup that never settles panics naming the last description rather than hanging.
async fn backup_when_available(router: &MultiServiceRouter, case: &str, backup_arn: &str) {
    let started = std::time::Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < ACTIVE_DEADLINE {
        let described = describe_backup(router, backup_arn).await;
        if described["BackupDetails"]["BackupStatus"] == json!("AVAILABLE") {
            return;
        }
        last = described;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {backup_arn} never became AVAILABLE within {ACTIVE_DEADLINE:?}, last {last}")
}

#[tokio::test]
async fn every_surface_that_describes_a_table_reports_the_maximum_it_carries() {
    // Rows differ in WHICH maximum the table carries — none, one dimension, both — and the
    // surfaces inside each row are what the rule generalises over: one table, described by
    // every operation that answers with a description of it.
    let rows: [(&str, Option<Value>, Value); 4] = [
        ("a table with no maximum", None, Value::Null),
        (
            "the read maximum alone",
            Some(json!({"MaxReadRequestUnits": 100})),
            json!({"MaxReadRequestUnits": 100, "MaxWriteRequestUnits": -1}),
        ),
        (
            "the write maximum alone",
            Some(json!({"MaxWriteRequestUnits": 7})),
            json!({"MaxReadRequestUnits": -1, "MaxWriteRequestUnits": 7}),
        ),
        (
            "both maxima, one at the documented minimum",
            Some(json!({"MaxReadRequestUnits": 1, "MaxWriteRequestUnits": 4})),
            json!({"MaxReadRequestUnits": 1, "MaxWriteRequestUnits": 4}),
        ),
    ];

    for (row, (case, maximum, expected)) in rows.into_iter().enumerate() {
        let router = emulator();
        let table = format!("Surfaces{row}");
        let mut reported: Vec<(&str, Value)> = Vec::new();

        // The create's own answer, and the reader a client polls after it.
        let created = create_table(&router, &table, "PAY_PER_REQUEST", maximum.clone()).await;
        reported.push(("CreateTable's TableDescription", block(&created)));
        let described = describe_when_active(&router, case, &table).await;
        reported.push(("DescribeTable's Table", block(&described)));

        // An update that moves a DIFFERENT setting entirely still describes the table, so
        // it reports the same block.
        let protected = update_table(
            &router,
            case,
            &table,
            json!({"DeletionProtectionEnabled": true}),
        )
        .await;
        reported.push(("UpdateTable's TableDescription", block(&protected)));

        // The record a backup of this table keeps of it, read back through both operations
        // that report a `BackupDescription`.
        let backup_arn = create_backup(&router, &table, &format!("backup-{row}")).await;
        let backed = describe_backup(&router, &backup_arn).await;
        reported.push((
            "DescribeBackup's SourceTableDetails",
            block(&backed["SourceTableDetails"]),
        ));
        backup_when_available(&router, case, &backup_arn).await;
        let removed = ok(&router, "DeleteBackup", json!({"BackupArn": backup_arn})).await;
        reported.push((
            "DeleteBackup's SourceTableDetails",
            block(&removed["BackupDescription"]["SourceTableDetails"]),
        ));

        // And the delete's answer, once the protection this suite turned on is off again.
        update_table(
            &router,
            case,
            &table,
            json!({"DeletionProtectionEnabled": false}),
        )
        .await;
        let deleted = ok(&router, "DeleteTable", json!({"TableName": table})).await;
        reported.push((
            "DeleteTable's TableDescription",
            block(&deleted["TableDescription"]),
        ));

        for (surface, answered) in reported {
            assert_eq!(
                answered, expected,
                "case {case:?} on {surface}: the block is reported exactly where a maximum is \
                 set, expected {expected}, got {answered:?}"
            );
        }
    }
}

/// One row of the test below: a table, how its maximum came to be in force, what the
/// backup taken of it must record, and the change the TABLE is put through afterwards.
struct Recorded {
    case: &'static str,
    billing_mode: &'static str,
    /// The `OnDemandThroughput` the table's create names, when the row names one.
    maximum: Option<Value>,
    /// The `UpdateTable` members applied BEFORE the backup is taken, when the row has the
    /// maximum arrive — or go — through an update.
    before: Option<Value>,
    /// The block the backup's `SourceTableDetails` must report, `Value::Null` for a row
    /// whose table carried no maximum.
    expected: Value,
    /// The `UpdateTable` members applied AFTER the backup, which the record cannot follow.
    later: Value,
}

#[tokio::test]
async fn a_backup_records_the_maximum_the_table_carried_when_it_was_taken() {
    // Rows differ in how the maximum — or the absence of one — came to be in force at the
    // instant of the backup: the capacity mode the table is in, whether its create named a
    // maximum, and whether an update set or removed one first. `later` is the change the
    // TABLE is then put through, which the record already taken cannot follow.
    let rows: [Recorded; 5] = [
        Recorded {
            case: "an on-demand table nobody capped",
            billing_mode: "PAY_PER_REQUEST",
            maximum: None,
            before: None,
            expected: Value::Null,
            later: json!({"OnDemandThroughput": {"MaxReadRequestUnits": 50}}),
        },
        Recorded {
            case: "a provisioned table, whose capacity is its ProvisionedThroughput",
            billing_mode: "PROVISIONED",
            maximum: None,
            before: None,
            expected: Value::Null,
            later: json!({
                "ProvisionedThroughput": {"ReadCapacityUnits": 7, "WriteCapacityUnits": 7},
            }),
        },
        Recorded {
            case: "a maximum the create named",
            billing_mode: "PAY_PER_REQUEST",
            maximum: Some(json!({"MaxReadRequestUnits": 100})),
            before: None,
            expected: json!({"MaxReadRequestUnits": 100, "MaxWriteRequestUnits": -1}),
            later: json!({"OnDemandThroughput": {"MaxReadRequestUnits": -1}}),
        },
        Recorded {
            case: "a maximum an update set after the create",
            billing_mode: "PAY_PER_REQUEST",
            maximum: None,
            before: Some(json!({"OnDemandThroughput": {"MaxWriteRequestUnits": 7}})),
            expected: json!({"MaxReadRequestUnits": -1, "MaxWriteRequestUnits": 7}),
            later: json!({"OnDemandThroughput": {"MaxWriteRequestUnits": 9}}),
        },
        Recorded {
            case: "a maximum an update removed before the backup",
            billing_mode: "PAY_PER_REQUEST",
            maximum: Some(json!({"MaxReadRequestUnits": 5, "MaxWriteRequestUnits": 6})),
            before: Some(json!({
                "OnDemandThroughput": {"MaxReadRequestUnits": -1, "MaxWriteRequestUnits": -1},
            })),
            expected: Value::Null,
            later: json!({"OnDemandThroughput": {"MaxReadRequestUnits": 9}}),
        },
    ];

    for (row, case_row) in rows.into_iter().enumerate() {
        let Recorded {
            case,
            billing_mode,
            maximum,
            before,
            expected,
            later,
        } = case_row;
        let router = emulator();
        let table = format!("Recorded{row}");
        create_table(&router, &table, billing_mode, maximum).await;
        describe_when_active(&router, case, &table).await;
        if let Some(before) = before {
            update_table(&router, case, &table, before).await;
        }

        let backup_arn = create_backup(&router, &table, &format!("recorded-{row}")).await;
        let backed = describe_backup(&router, &backup_arn).await;
        let source = &backed["SourceTableDetails"];
        assert_eq!(
            (block(source), source["BillingMode"].clone()),
            (expected.clone(), json!(billing_mode)),
            "case {case:?}: the backup records the setting in force when it was taken, \
             expected {expected} in {billing_mode}, got {backed}"
        );

        // The record is frozen: moving the TABLE's maximum afterwards changes the table's
        // own description and leaves the backup reporting what it recorded.
        let moved = update_table(&router, case, &table, later).await;
        let backed = describe_backup(&router, &backup_arn).await;
        assert_eq!(
            block(&backed["SourceTableDetails"]),
            expected,
            "case {case:?}: a later change to the table leaves the backup's record alone, the \
             table now reporting {}, got {backed}",
            block(&moved)
        );
    }
}
