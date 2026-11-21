// SPDX-License-Identifier: CC0-1.0

//! Which GENERATION of a table's capacity figures the answer to an accepted `UpdateTable`
//! carries.
//!
//! The rule under test: `UpdateTable` is asynchronous ("UpdateTable is an asynchronous
//! operation; while it's executing, the table status changes from ACTIVE to UPDATING.
//! While it's UPDATING, you can't issue another UpdateTable request. Use DescribeTable to
//! monitor the table's status" — the `UpdateTable` doc), and the `TableDescription` it
//! answers with describes the table, so every capacity figure in it is the one the table
//! is SERVING when the call returns: the provisioned pair, the count of decreases already
//! applied ("The number of provisioned throughput decreases for this table during this UTC
//! calendar day" — `ProvisionedThroughputDescription$NumberOfDecreasesToday`) and the warm
//! pair ("the minimum provisioned capacity maintained for immediate data access" —
//! `TableWarmThroughputDescription`), at the table and at each index it already carried.
//! The figures the request names are what the reads AFTER the move report. An update that
//! moves the capacity MODE is the exception, tabled in
//! `update_table_capacity_mode_switch_figures.rs`: the mode is in force when the call
//! returns and the provisioned pair belongs to the mode, so that answer carries the new
//! mode's figures.
//!
//! The rows below span the class the rule generalises over rather than one capacity move:
//! each direction a provisioned pair can move, a capacity withdrawn by a capacity-mode
//! switch, a warm pair the request names, an index's own capacity moved, and
//! an index CREATED by the same call — which has no earlier generation, so it reports the
//! figures it was created with.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code};

/// The capacity the table every row updates is created with.
const CREATED_READ: i64 = 6;
const CREATED_WRITE: i64 = 4;

/// The capacity its one global secondary index is created with.
const CREATED_INDEX_READ: i64 = 3;
const CREATED_INDEX_WRITE: i64 = 2;

/// One accepted `UpdateTable`, with the figures its own answer must report and the figures
/// the settled table must report.
struct Row {
    /// The row this case's assertions name.
    case: &'static str,
    /// The request, minus the `TableName` every row carries.
    update: Value,
    /// JSON pointers into the `TableDescription`, each with the value the ANSWER to the
    /// update must carry — the figure in force while the move it started is in flight.
    in_force: &'static [(&'static str, i64)],
    /// The same pointers with the value the SETTLED table must carry.
    settled: &'static [(&'static str, i64)],
}

/// The table's own provisioned pair, as a description reports it.
const TABLE_READ: &str = "/ProvisionedThroughput/ReadCapacityUnits";
const TABLE_WRITE: &str = "/ProvisionedThroughput/WriteCapacityUnits";
const TABLE_DECREASES: &str = "/ProvisionedThroughput/NumberOfDecreasesToday";
const TABLE_WARM_READ: &str = "/WarmThroughput/ReadUnitsPerSecond";
const TABLE_WARM_WRITE: &str = "/WarmThroughput/WriteUnitsPerSecond";

/// The index's, which a description reports in the order the table carries them.
const INDEX_READ: &str = "/GlobalSecondaryIndexes/0/ProvisionedThroughput/ReadCapacityUnits";
const INDEX_WRITE: &str = "/GlobalSecondaryIndexes/0/ProvisionedThroughput/WriteCapacityUnits";
const INDEX_WARM_READ: &str = "/GlobalSecondaryIndexes/0/WarmThroughput/ReadUnitsPerSecond";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "both dimensions raised",
            update: json!({
                "ProvisionedThroughput": {"ReadCapacityUnits": 11, "WriteCapacityUnits": 9},
            }),
            in_force: &[
                (TABLE_READ, CREATED_READ),
                (TABLE_WRITE, CREATED_WRITE),
                (TABLE_DECREASES, 0),
                // A pair nobody named follows the capacity, so it is in flight with it.
                (TABLE_WARM_READ, CREATED_READ),
                (TABLE_WARM_WRITE, CREATED_WRITE),
            ],
            settled: &[
                (TABLE_READ, 11),
                (TABLE_WRITE, 9),
                (TABLE_DECREASES, 0),
                (TABLE_WARM_READ, 11),
                (TABLE_WARM_WRITE, 9),
            ],
        },
        Row {
            case: "both dimensions lowered",
            update: json!({
                "ProvisionedThroughput": {"ReadCapacityUnits": 2, "WriteCapacityUnits": 1},
            }),
            in_force: &[
                (TABLE_READ, CREATED_READ),
                (TABLE_WRITE, CREATED_WRITE),
                // The decrease this call accepted has not been applied yet.
                (TABLE_DECREASES, 0),
            ],
            settled: &[
                (TABLE_READ, 2),
                (TABLE_WRITE, 1),
                (TABLE_DECREASES, 1),
                // What was being kept warm stays warm: a capacity move only raises it.
                (TABLE_WARM_READ, CREATED_READ),
                (TABLE_WARM_WRITE, CREATED_WRITE),
            ],
        },
        Row {
            case: "one dimension raised and the other lowered",
            update: json!({
                "ProvisionedThroughput": {"ReadCapacityUnits": 12, "WriteCapacityUnits": 1},
            }),
            in_force: &[
                (TABLE_READ, CREATED_READ),
                (TABLE_WRITE, CREATED_WRITE),
                (TABLE_DECREASES, 0),
                (TABLE_WARM_READ, CREATED_READ),
            ],
            settled: &[
                (TABLE_READ, 12),
                (TABLE_WRITE, 1),
                (TABLE_DECREASES, 1),
                (TABLE_WARM_READ, 12),
            ],
        },
        Row {
            case: "the capacity withdrawn by a switch to on-demand",
            update: json!({"BillingMode": "PAY_PER_REQUEST"}),
            // The capacity MODE is in force when the call returns, and the provisioned pair
            // belongs to the mode: "on-demand mode does not use provisioned throughput", so
            // the table and its index report none from this answer on.
            in_force: &[
                (TABLE_READ, 0),
                (TABLE_WRITE, 0),
                (TABLE_DECREASES, 0),
                (INDEX_READ, 0),
                (INDEX_WRITE, 0),
            ],
            settled: &[
                // Withdrawing the capacity spends none of the day's counted decreases.
                (TABLE_READ, 0),
                (TABLE_WRITE, 0),
                (TABLE_DECREASES, 0),
                (INDEX_READ, 0),
                (INDEX_WRITE, 0),
                (TABLE_WARM_READ, CREATED_READ),
            ],
        },
        Row {
            case: "a warm pair the request names",
            update: json!({
                "WarmThroughput": {"ReadUnitsPerSecond": 30_000, "WriteUnitsPerSecond": 9_000},
            }),
            in_force: &[
                (TABLE_WARM_READ, CREATED_READ),
                (TABLE_WARM_WRITE, CREATED_WRITE),
            ],
            settled: &[(TABLE_WARM_READ, 30_000), (TABLE_WARM_WRITE, 9_000)],
        },
        Row {
            case: "an index's own capacity moved",
            update: json!({
                "GlobalSecondaryIndexUpdates": [{"Update": {
                    "IndexName": "ByOwner",
                    "ProvisionedThroughput": {"ReadCapacityUnits": 7, "WriteCapacityUnits": 7},
                }}],
            }),
            in_force: &[
                (INDEX_READ, CREATED_INDEX_READ),
                (INDEX_WRITE, CREATED_INDEX_WRITE),
                (INDEX_WARM_READ, CREATED_INDEX_READ),
                // The table's own capacity is not what this call moves.
                (TABLE_READ, CREATED_READ),
            ],
            settled: &[
                (INDEX_READ, 7),
                (INDEX_WRITE, 7),
                (INDEX_WARM_READ, 7),
                (TABLE_READ, CREATED_READ),
            ],
        },
        Row {
            case: "an index created by the same call",
            update: json!({
                "AttributeDefinitions": [{"AttributeName": "Tag", "AttributeType": "S"}],
                "GlobalSecondaryIndexUpdates": [{"Create": {
                    "IndexName": "ByTag",
                    "KeySchema": [{"AttributeName": "Tag", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                    "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
                }}],
            }),
            in_force: &[
                // A created index has no earlier generation, so it reports the figures it
                // was created with; the index the table already carried reports its own.
                ("/GlobalSecondaryIndexes/1/ProvisionedThroughput/ReadCapacityUnits", 5),
                (INDEX_READ, CREATED_INDEX_READ),
                (TABLE_READ, CREATED_READ),
            ],
            settled: &[
                ("/GlobalSecondaryIndexes/1/ProvisionedThroughput/ReadCapacityUnits", 5),
                (INDEX_READ, CREATED_INDEX_READ),
                (TABLE_READ, CREATED_READ),
            ],
        },
    ]
}

/// A table name derived from the row, so each row drives a table of its own.
fn table_name(case: &str) -> String {
    let mut name = String::from("cap-");
    name.extend(case.chars().map(|character| {
        if character.is_ascii_alphanumeric() {
            character
        } else {
            '-'
        }
    }));
    name
}

/// Create the `PROVISIONED` table every row updates, with one global secondary index to
/// move and an attribute a new index can be keyed on, and wait for it to settle.
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
        "case {case:?}: the table the row updates must be created, got {status} with {created}"
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
async fn an_accepted_capacity_move_reports_the_capacity_still_in_force() {
    for row in rows() {
        let router = emulator();
        let name = table_name(row.case);
        create_table(&router, row.case, &name).await;

        let mut request = row.update.clone();
        request["TableName"] = json!(name);
        let (status, updated) = call(&router, "UpdateTable", request).await;
        assert_eq!(
            (status, updated["TableDescription"]["TableStatus"].clone()),
            (200, json!("UPDATING")),
            "case {:?}: a capacity move is accepted with the table UPDATING, got {status} \
             with {updated}",
            row.case
        );
        assert_figures(
            row.case,
            "the answer to the move",
            &updated["TableDescription"],
            row.in_force,
        );

        // The clock settles the move on its own, and the figures it left are what a read of
        // the table reports from then on.
        let settled = describe_when_active(&router, row.case, &name).await;
        assert_figures(row.case, "the settled table", &settled, row.settled);
    }
}

#[tokio::test]
async fn a_warm_pair_a_move_is_in_flight_for_reports_updating_over_the_pair_in_force() {
    // The status beside the figures is the MOVE's, not the figures': a pair a change is in
    // flight for reports `UPDATING` while it still reports the pair in force, and `ACTIVE`
    // once the pair the request named is the one in force ("The status can only be UPDATING
    // or ACTIVE" — `TableWarmThroughputDescription$Status`).
    let router = emulator();
    let name = "cap-warm-status";
    create_table(&router, "a warm pair in flight", name).await;

    let (status, updated) = call(
        &router,
        "UpdateTable",
        json!({
            "TableName": name,
            "WarmThroughput": {"ReadUnitsPerSecond": 24_000, "WriteUnitsPerSecond": 8_000},
        }),
    )
    .await;
    assert_eq!(
        (
            status,
            updated["TableDescription"]["WarmThroughput"].clone()
        ),
        (
            200,
            json!({
                "ReadUnitsPerSecond": CREATED_READ,
                "WriteUnitsPerSecond": CREATED_WRITE,
                "Status": "UPDATING",
            })
        ),
        "case \"a warm pair in flight\": expected the pair in force reported UPDATING, got \
         {status} with {updated}"
    );

    let settled = describe_when_active(&router, "a warm pair in flight", name).await;
    assert_eq!(
        settled["WarmThroughput"],
        json!({
            "ReadUnitsPerSecond": 24_000,
            "WriteUnitsPerSecond": 8_000,
            "Status": "ACTIVE",
        }),
        "case \"a warm pair in flight\": expected the named pair in force once it settled, \
         got {settled}"
    );
}

#[tokio::test]
async fn a_move_the_service_refuses_leaves_every_figure_where_it_was() {
    // The other half of the rule: a refused move reports nothing of itself, so the figures
    // a reader sees are the ones in force before it — a second move inside the span of the
    // first is the refusal every row of the table above earns ("While it's UPDATING, you
    // can't issue another UpdateTable request").
    let router = emulator();
    let name = "cap-refused";
    create_table(&router, "a refused second move", name).await;

    let (status, accepted) = call(
        &router,
        "UpdateTable",
        json!({
            "TableName": name,
            "ProvisionedThroughput": {"ReadCapacityUnits": 20, "WriteCapacityUnits": 20},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a refused second move\": the first move is accepted, got {status} with \
         {accepted}"
    );
    let (status, refused) = call(
        &router,
        "UpdateTable",
        json!({
            "TableName": name,
            "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": 1},
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ResourceInUseException"),
        "case \"a refused second move\": a second move inside the span conflicts, got \
         {status} with {refused}"
    );

    let settled = describe_when_active(&router, "a refused second move", name).await;
    assert_figures(
        "a refused second move",
        "the settled table",
        &settled,
        &[
            (TABLE_READ, 20),
            (TABLE_WRITE, 20),
            (TABLE_DECREASES, 0),
        ],
    );
}
