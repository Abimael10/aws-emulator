// SPDX-License-Identifier: CC0-1.0

//! What a `Scan` CHARGES: the read capacity it reports for the items it examined.
//!
//! The rule these tests pin:
//!
//! > a scan's `ConsumedCapacity.CapacityUnits` — "the total number of capacity units
//! > consumed by the operation" — is the sum over the items the scan EXAMINED of each
//! > item's size rounded up to a 4 KB read unit, halved for an eventually consistent read
//! > ("one read capacity unit represents one strongly consistent read per second ... for an
//! > item up to 4 KB" and "two eventually consistent reads per second", the Developer
//! > Guide's "Read/write capacity mode"); a scan that examined nothing still spends the one
//! > unit a read takes at least, and the figure is reported only when the request asks for
//! > it.
//!
//! The rows span what the rule generalises over: how many items the scan examines, how
//! large each of them is (inside one read unit, and over it), and whether the read is
//! strongly or eventually consistent. A `FilterExpression` is carried by one row as well,
//! because a filter runs after the page is read and so cannot reduce what was charged.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{describe_when_active, emulator, ok};

/// The bytes of `Fill` that put one item OVER a 4 KB read unit and inside the second: the
/// unit is 4096 bytes, and this item's other attributes add fewer than the slack left here.
const OVER_ONE_UNIT: usize = 5_000;

/// One table of `items` items, each carrying `fill` bytes beside its key.
async fn table_of(router: &MultiServiceRouter, name: &str, items: usize, fill: usize) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, name, name).await;
    for index in 0..items {
        let mut item = json!({"Part": {"S": format!("item-{index}")}});
        if fill > 0 {
            item["Fill"] = json!({"S": "x".repeat(fill)});
        }
        ok(
            router,
            "PutItem",
            json!({"TableName": name, "Item": item}),
        )
        .await;
    }
}

/// One row of the charging table: the table's shape, what the scan asks for, and the units
/// it must then report.
struct Charged {
    case: &'static str,
    table: &'static str,
    /// How many items the table holds, and how many bytes each carries beside its key.
    items: usize,
    fill: usize,
    consistent: bool,
    /// A `FilterExpression` the scan carries, which cannot change the charge.
    filtered: bool,
    units: f64,
}

#[tokio::test]
async fn a_scan_is_charged_a_read_unit_for_every_item_it_examined() {
    let router = emulator();
    // Each row names the units its scan must report: one unit per item inside a 4 KB read
    // unit, two per item over it, and half of that for an eventually consistent read.
    let rows = [
        Charged {
            case: "one small item, eventually consistent",
            table: "Charged.one",
            items: 1,
            fill: 0,
            consistent: false,
            filtered: false,
            units: 0.5,
        },
        Charged {
            case: "three small items, eventually consistent",
            table: "Charged.three",
            items: 3,
            fill: 0,
            consistent: false,
            filtered: false,
            units: 1.5,
        },
        Charged {
            case: "three small items, strongly consistent",
            table: "Charged.three",
            items: 3,
            fill: 0,
            consistent: true,
            filtered: false,
            units: 3.0,
        },
        Charged {
            case: "three small items, a filter that keeps none",
            table: "Charged.three",
            items: 3,
            fill: 0,
            consistent: true,
            filtered: true,
            units: 3.0,
        },
        Charged {
            case: "one item over a read unit, strongly consistent",
            table: "Charged.large",
            items: 1,
            fill: OVER_ONE_UNIT,
            consistent: true,
            filtered: false,
            units: 2.0,
        },
        Charged {
            case: "two items over a read unit, eventually consistent",
            table: "Charged.larger",
            items: 2,
            fill: OVER_ONE_UNIT,
            consistent: false,
            filtered: false,
            units: 2.0,
        },
        Charged {
            case: "a table holding nothing",
            table: "Charged.empty",
            items: 0,
            fill: 0,
            consistent: false,
            filtered: false,
            units: 0.5,
        },
    ];

    // The rows share tables where their shape is the same, so each is seeded once.
    let mut seeded: Vec<&str> = Vec::new();
    for row in &rows {
        if !seeded.contains(&row.table) {
            table_of(&router, row.table, row.items, row.fill).await;
            seeded.push(row.table);
        }
    }

    for row in &rows {
        let mut request = json!({
            "TableName": row.table,
            "ConsistentRead": row.consistent,
            "ReturnConsumedCapacity": "TOTAL",
        });
        if row.filtered {
            request["FilterExpression"] = json!("#f = :absent");
            request["ExpressionAttributeNames"] = json!({"#f": "Part"});
            request["ExpressionAttributeValues"] =
                json!({":absent": {"S": "no item carries this"}});
        }
        let scanned = ok(&router, "Scan", request).await;
        assert_eq!(
            (
                scanned["ConsumedCapacity"]["CapacityUnits"].clone(),
                // `TOTAL` asks for "only the aggregate ConsumedCapacity for the
                // operation" (the `ReturnConsumedCapacity` doc), so the per-kind figure
                // that itemises it is not part of this answer.
                scanned["ConsumedCapacity"].get("ReadCapacityUnits").cloned(),
                scanned["ScannedCount"].clone(),
            ),
            (json!(row.units), None, json!(row.items)),
            "case {:?}: expected {} read unit(s) over {} item(s), got {scanned}",
            row.case,
            row.units,
            row.items
        );
    }
}

#[tokio::test]
async fn the_charge_is_reported_only_when_the_request_asks_for_it() {
    let router = emulator();
    table_of(&router, "Charged.asked", 2, 0).await;
    // Each row is a `ReturnConsumedCapacity` value and whether the response then carries a
    // figure at all: "ConsumedCapacity is only returned if the ReturnConsumedCapacity
    // parameter was specified" (the `Scan` doc).
    let rows: [(&str, Option<&str>, Option<f64>); 4] = [
        ("the member left out", None, None),
        ("NONE", Some("NONE"), None),
        ("TOTAL", Some("TOTAL"), Some(1.0)),
        ("INDEXES", Some("INDEXES"), Some(1.0)),
    ];
    for (case, requested, expected) in rows {
        let mut request = json!({"TableName": "Charged.asked"});
        if let Some(requested) = requested {
            request["ReturnConsumedCapacity"] = json!(requested);
        }
        let scanned = ok(&router, "Scan", request).await;
        match expected {
            None => assert!(
                scanned.get("ConsumedCapacity").is_none_or(Value::is_null),
                "case {case:?}: no capacity is reported unless it was asked for, got {scanned}"
            ),
            Some(units) => assert_eq!(
                scanned["ConsumedCapacity"]["CapacityUnits"],
                json!(units),
                "case {case:?}: expected {units} read unit(s), got {scanned}"
            ),
        }
    }
    // The `INDEXES` arm breaks the same figure out per table, so the table's own line
    // carries what the operation was charged.
    let broken_out = ok(
        &router,
        "Scan",
        json!({"TableName": "Charged.asked", "ReturnConsumedCapacity": "INDEXES"}),
    )
    .await;
    assert_eq!(
        broken_out["ConsumedCapacity"]["Table"]["CapacityUnits"],
        json!(1.0),
        "case the INDEXES arm: the table's own line carries the units, got {broken_out}"
    );
}
