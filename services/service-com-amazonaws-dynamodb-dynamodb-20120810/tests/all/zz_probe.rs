// SPDX-License-Identifier: CC0-1.0

//! What a WRITE of an item costs, probed across the 1 KB capacity unit on items whose
//! bytes are not all strings.
//!
//! RULE: "one write capacity unit represents one write per second for an item up to 1 KB
//! in size" ("Read/write capacity mode"), so a write costs the item's size rounded up to
//! the kilobyte — and the size is the item's FULL attribute encoding, every member of
//! every type included: a number by the digits its canonical form spells, a binary value
//! by its raw bytes, and a set by the bytes of each of its members. The item a DELETE
//! removes is sized the same way, because what the write costs is the item it moved.
//!
//! The rows straddle the unit boundary on an item built from a number, a binary value and
//! a binary set as well as a string, so a member whose bytes were not counted — or counted
//! by its wire text rather than by what the store holds — moves the reported units.

use serde_json::json;

use crate::emulator::{describe_when_active, emulator, ok};

/// One write capacity unit covers 1 KB.
const WRITE_UNIT_BYTES: usize = 1024;

/// The bytes the fixed part of the probe item takes: `pk` and its two-character key, `n`
/// and a five-digit number (three bytes of digits plus the base byte), `b` and three raw
/// bytes of binary, `bs` and the two three-byte members of a binary set, and the name of
/// the padding attribute.
const FIXED_BYTES: usize = 2 + 2 + 1 + 4 + 1 + 3 + 2 + 3 + 3 + 1;

#[tokio::test]
async fn a_write_is_charged_the_whole_encoding_of_the_item_it_moved() {
    let router = emulator();
    let name = "Weighed";
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": name,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        }),
    )
    .await;
    describe_when_active(&router, "the table the write probes run against", name).await;

    // (case, the item's size in bytes, the units a write of it costs)
    let rows: [(&str, usize, f64); 4] = [
        ("one byte inside the unit", WRITE_UNIT_BYTES - 1, 1.0),
        ("exactly one unit", WRITE_UNIT_BYTES, 1.0),
        ("one byte past the unit", WRITE_UNIT_BYTES + 1, 2.0),
        ("two whole units", WRITE_UNIT_BYTES * 2, 2.0),
    ];

    for (position, (case, bytes, units)) in rows.into_iter().enumerate() {
        let key = format!("k{position}");
        let item = json!({
            "pk": {"S": key.clone()},
            "n": {"N": "12345"},
            "b": {"B": "AAAA"},
            "bs": {"BS": ["AAAB", "AAAC"]},
            "v": {"S": "p".repeat(bytes - FIXED_BYTES)},
        });

        let written = ok(
            &router,
            "PutItem",
            json!({
                "TableName": name,
                "Item": item,
                "ReturnConsumedCapacity": "TOTAL",
            }),
        )
        .await;
        assert_eq!(
            written["ConsumedCapacity"]["CapacityUnits"].as_f64(),
            Some(units),
            "case {case:?}: a write of {bytes} bytes costs {units} unit(s), got {written}"
        );

        // The delete is charged the item it removed, which is the same item.
        let removed = ok(
            &router,
            "DeleteItem",
            json!({
                "TableName": name,
                "Key": {"pk": {"S": key.clone()}},
                "ReturnConsumedCapacity": "TOTAL",
            }),
        )
        .await;
        assert_eq!(
            removed["ConsumedCapacity"]["CapacityUnits"].as_f64(),
            Some(units),
            "case {case:?}: deleting the {bytes}-byte item costs {units} unit(s), got \
             {removed}"
        );
    }
}
