// SPDX-License-Identifier: CC0-1.0

//! Which generation of a table's capacity figures the answer to an `UpdateTable` that moves
//! the CAPACITY MODE carries.
//!
//! The rule under test: an `UpdateTable` that resolves to a capacity mode other than the one
//! the table is in reports the NEW mode's capacity figures — none for a table moved onto
//! on-demand capacity ("on-demand mode does not use provisioned throughput", so the pair
//! reads zero for the table and for every index), and the pair the update set for one moved
//! onto provisioned capacity ("When switching from pay-per-request to provisioned capacity,
//! initial provisioned capacity values must be set" — `UpdateTable$BillingMode`). The mode a
//! request resolves to is in force from the moment the call returns, which the same
//! description reports in its `BillingModeSummary` and `TableThroughputModeSummary`, and the
//! provisioned pair belongs to the mode.
//!
//! The contrast rows are the moves that leave the mode where it is: those answer with the
//! figures still in force while the move they started is in flight, which is the rule
//! `update_table_capacity_generation.rs` tables.
//!
//! The rows vary what the rule generalises over: the DIRECTION of the move (onto on-demand
//! and back onto provisioned), the MEMBER the mode arrives in (`BillingMode` and the
//! `TableThroughputMode` spelling of the same setting), whether the request names a
//! provisioned pair beside the mode, and whether the table carries a global secondary index
//! whose capacity moves with it — against a capacity move inside one mode and a request that
//! re-states the mode already in force, neither of which moves the mode.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator};

/// The capacity the provisioned table each row starts from is created with.
const CREATED_READ: i64 = 6;
const CREATED_WRITE: i64 = 4;

/// The capacity its one global secondary index is created with.
const CREATED_INDEX_READ: i64 = 3;
const CREATED_INDEX_WRITE: i64 = 2;

/// The pair a row that moves the table back onto provisioned capacity sets.
const RESTORED_READ: i64 = 5;
const RESTORED_WRITE: i64 = 2;
const RESTORED_INDEX_READ: i64 = 7;
const RESTORED_INDEX_WRITE: i64 = 7;

/// The members of a `TableDescription` these rows read.
const TABLE_READ: &str = "/ProvisionedThroughput/ReadCapacityUnits";
const TABLE_WRITE: &str = "/ProvisionedThroughput/WriteCapacityUnits";
const INDEX_READ: &str = "/GlobalSecondaryIndexes/0/ProvisionedThroughput/ReadCapacityUnits";
const INDEX_WRITE: &str = "/GlobalSecondaryIndexes/0/ProvisionedThroughput/WriteCapacityUnits";

/// One row: a request, and the figures its own answer must carry.
struct Row {
    case: &'static str,
    /// Whether the table is moved onto on-demand capacity before the request below runs.
    start_on_demand: bool,
    /// The request, minus the `TableName` every row carries.
    update: Value,
    /// JSON pointers into the answered `TableDescription`, each with the figure it must
    /// report.
    answered: &'static [(&'static str, i64)],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "onto on-demand, by BillingMode",
            start_on_demand: false,
            update: json!({"BillingMode": "PAY_PER_REQUEST"}),
            answered: &[
                (TABLE_READ, 0),
                (TABLE_WRITE, 0),
                (INDEX_READ, 0),
                (INDEX_WRITE, 0),
            ],
        },
        Row {
            case: "onto on-demand, by TableThroughputMode",
            start_on_demand: false,
            update: json!({"TableThroughputMode": "PAY_PER_REQUEST"}),
            answered: &[
                (TABLE_READ, 0),
                (TABLE_WRITE, 0),
                (INDEX_READ, 0),
                (INDEX_WRITE, 0),
            ],
        },
        Row {
            case: "back onto provisioned, by BillingMode",
            start_on_demand: true,
            update: json!({
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": RESTORED_READ, "WriteCapacityUnits": RESTORED_WRITE,
                },
                "GlobalSecondaryIndexUpdates": [{"Update": {
                    "IndexName": "ByOwner",
                    "ProvisionedThroughput": {
                        "ReadCapacityUnits": RESTORED_INDEX_READ,
                        "WriteCapacityUnits": RESTORED_INDEX_WRITE,
                    },
                }}],
            }),
            answered: &[
                (TABLE_READ, RESTORED_READ),
                (TABLE_WRITE, RESTORED_WRITE),
                (INDEX_READ, RESTORED_INDEX_READ),
                (INDEX_WRITE, RESTORED_INDEX_WRITE),
            ],
        },
        Row {
            case: "back onto provisioned, by TableThroughputMode",
            start_on_demand: true,
            update: json!({
                "TableThroughputMode": "PROVISIONED",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": RESTORED_READ, "WriteCapacityUnits": RESTORED_WRITE,
                },
                "GlobalSecondaryIndexUpdates": [{"Update": {
                    "IndexName": "ByOwner",
                    "ProvisionedThroughput": {
                        "ReadCapacityUnits": RESTORED_INDEX_READ,
                        "WriteCapacityUnits": RESTORED_INDEX_WRITE,
                    },
                }}],
            }),
            answered: &[
                (TABLE_READ, RESTORED_READ),
                (TABLE_WRITE, RESTORED_WRITE),
                (INDEX_READ, RESTORED_INDEX_READ),
                (INDEX_WRITE, RESTORED_INDEX_WRITE),
            ],
        },
        // The contrast rows: the mode does not move, so the answer carries the figures still
        // in force.
        Row {
            case: "a capacity move inside the provisioned mode",
            start_on_demand: false,
            update: json!({
                "ProvisionedThroughput": {"ReadCapacityUnits": 12, "WriteCapacityUnits": 11},
            }),
            answered: &[
                (TABLE_READ, CREATED_READ),
                (TABLE_WRITE, CREATED_WRITE),
                (INDEX_READ, CREATED_INDEX_READ),
                (INDEX_WRITE, CREATED_INDEX_WRITE),
            ],
        },
        Row {
            case: "the mode already in force re-stated beside a capacity move",
            start_on_demand: false,
            update: json!({
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {"ReadCapacityUnits": 9, "WriteCapacityUnits": 8},
            }),
            answered: &[
                (TABLE_READ, CREATED_READ),
                (TABLE_WRITE, CREATED_WRITE),
                (INDEX_READ, CREATED_INDEX_READ),
                (INDEX_WRITE, CREATED_INDEX_WRITE),
            ],
        },
    ]
}

/// A table name derived from the row, so each row drives a table of its own.
fn table_name(case: &str) -> String {
    let mut name = String::from("mode-");
    name.extend(case.chars().map(|character| {
        if character.is_ascii_alphanumeric() {
            character
        } else {
            '-'
        }
    }));
    name
}

/// Create the provisioned table with one global secondary index every row starts from, and
/// wait for it to settle.
async fn create_table(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, created) = call(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "Part", "AttributeType": "S"},
                {"AttributeName": "Owner", "AttributeType": "S"},
            ],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {
                "ReadCapacityUnits": CREATED_READ, "WriteCapacityUnits": CREATED_WRITE,
            },
            "GlobalSecondaryIndexes": [{
                "IndexName": "ByOwner",
                "KeySchema": [{"AttributeName": "Owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": CREATED_INDEX_READ,
                    "WriteCapacityUnits": CREATED_INDEX_WRITE,
                },
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the table the row starts from must be created, got {status} with \
         {created}"
    );
    describe_when_active(router, case, name).await;
}

/// Hold `description` to every figure `expected` names.
fn assert_figures(case: &str, origin: &str, description: &Value, expected: &[(&str, i64)]) {
    for (pointer, units) in expected {
        assert_eq!(
            description.pointer(pointer),
            Some(&json!(units)),
            "case {case:?}: expected {origin} to report {pointer} as {units}, got {description}"
        );
    }
}

#[tokio::test]
async fn a_capacity_mode_move_reports_the_new_modes_figures() {
    for row in rows() {
        let router = emulator();
        let name = table_name(row.case);
        create_table(&router, row.case, &name).await;

        if row.start_on_demand {
            let (status, switched) = call(
                &router,
                "UpdateTable",
                json!({"TableName": name, "BillingMode": "PAY_PER_REQUEST"}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: the table must reach on-demand capacity first, got {status} with \
                 {switched}",
                row.case
            );
            // The switch's own move has to settle before the row's request is accepted:
            // "While it's UPDATING, you can't issue another UpdateTable request."
            describe_when_active(&router, row.case, &name).await;
        }

        let mut request = row.update.clone();
        request["TableName"] = json!(name);
        let (status, updated) = call(&router, "UpdateTable", request).await;
        assert_eq!(
            status, 200,
            "case {:?}: the update must be accepted, got {status} with {updated}",
            row.case
        );
        assert_figures(
            row.case,
            "the answer to the update",
            &updated["TableDescription"],
            row.answered,
        );

        // The mode the request resolved to is reported beside those figures, and the settled
        // table reports the figures the move left — which for a mode move are the ones its
        // own answer already carried.
        let settled = describe_when_active(&router, row.case, &name).await;
        if !row.start_on_demand && row.update.get("ProvisionedThroughput").is_none() {
            assert_figures(
                row.case,
                "the settled table",
                &settled,
                &[(TABLE_READ, 0), (TABLE_WRITE, 0)],
            );
        }
    }
}

#[tokio::test]
async fn the_answer_to_a_mode_move_reports_the_mode_it_resolved_to() {
    // The mode is in force when the call returns, which is why the figures beside it are the
    // new mode's: both halves of that are read off the SAME description.
    let router = emulator();
    let name = "mode-summary-generation";
    create_table(&router, "the mode summary", name).await;

    let (status, switched) = call(
        &router,
        "UpdateTable",
        json!({"TableName": name, "BillingMode": "PAY_PER_REQUEST"}),
    )
    .await;
    let description = &switched["TableDescription"];
    assert_eq!(
        (
            status,
            description["BillingModeSummary"]["BillingMode"].clone(),
            description["ProvisionedThroughput"]["ReadCapacityUnits"].clone(),
        ),
        (200, json!("PAY_PER_REQUEST"), json!(0)),
        "case \"the mode summary\": expected the new mode beside its own figures, got \
         {status} with {switched}"
    );

    let settled = describe_when_active(&router, "the mode summary", name).await;
    assert_eq!(
        (
            settled["BillingModeSummary"]["BillingMode"].clone(),
            settled["ProvisionedThroughput"]["ReadCapacityUnits"].clone(),
        ),
        (json!("PAY_PER_REQUEST"), json!(0)),
        "case \"the mode summary\": the settled table reports the same, got {settled}"
    );
}
