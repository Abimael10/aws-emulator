// SPDX-License-Identifier: CC0-1.0

//! How WIDE a stored `N` attribute value is, which is what every byte budget this service
//! measures an item against is built out of.
//!
//! The rule pinned here: a stored number is a decimal exponent plus a run of digit PAIRS,
//! and the pairs are aligned on the DECIMAL POINT — the integral digits pair off leftwards
//! from the point, the fractional ones rightwards — so a number costs "1 byte per two
//! significant digits" "plus 1 byte" (the Developer Guide's "Item sizes and formats"),
//! counted over the pairs its own point aligns it into, and one byte more when it is
//! negative. A pair of zeros at either end is not stored at all, because the exponent
//! states it instead ("Leading and trailing zeroes are trimmed" — the `AttributeValue$N`
//! numeric domain), while a zero between two significant digits is part of the pair it
//! falls in. The alignment is what makes the rule observable: a whole number's trailing
//! zeros pair off and fall away, and the SAME integral digits keep theirs once a fraction
//! follows, because the point moves and the pairs realign.
//!
//! The boundaries that width is read at, all three fed by it:
//!
//! * the documented item ceiling — "The maximum item size in DynamoDB is 400 KB" ("Service,
//!   Account, and Table Quotas") — where an item sized exactly to it is stored and reads
//!   back whole, the number round-tripping as the one decimal form the store holds it in,
//!   and an item one byte past it is refused HTTP 400 `ValidationException` with the fixed
//!   sentence that quota states, leaving nothing behind;
//! * the write capacity unit, "one write request unit ... for an item up to 1 KB in size"
//!   ("Read/write capacity mode");
//! * the read capacity unit, "one strongly consistent read request ... for an item up to
//!   4 KB in size" (the same).
//!
//! The rule is asserted over a SET of literals spanning what it generalises over — the
//! digit count's parity, a whole number's trailing zeros against the same digits followed
//! by a fraction, an interior zero, leading zeros, an exponent, the sign, zero itself and
//! the documented 38-digit precision ceiling at both alignments — and over the three
//! places a number can sit in an item (on its own, inside a list, inside a number set),
//! because the width belongs to the value's TYPE and not to where it sits.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// "The maximum item size in DynamoDB is 400 KB" ("Service, Account, and Table Quotas").
const MAX_ITEM_BYTES: usize = 400 * 1024;

/// The fixed sentence a write past that ceiling is refused with.
const TOO_LARGE: &str = "Item size has exceeded the maximum allowed size";

/// The bytes one write capacity unit covers ("Read/write capacity mode").
const WRITE_UNIT_BYTES: usize = 1024;

/// The bytes one strongly consistent read capacity unit covers (the same).
const READ_UNIT_BYTES: usize = 4 * 1024;

/// One row of the catalogue: a literal, the decimal form the store holds it in, and the
/// bytes storing it is accounted for.
///
/// Every width here is computed from the rule in this module's docs — the digit pairs the
/// literal's own decimal point aligns it into, plus the exponent byte, plus the sign —
/// and none of them is read off a recorded response.
struct Width {
    case: &'static str,
    literal: &'static str,
    /// The one decimal form every read of the stored value reports.
    stored: &'static str,
    bytes: usize,
}

/// The catalogue.
fn widths() -> Vec<Width> {
    vec![
        Width {
            case: "one digit fills one pair",
            literal: "7",
            stored: "7",
            bytes: 2,
        },
        Width {
            case: "two digits fill the same pair",
            literal: "12",
            stored: "12",
            bytes: 2,
        },
        Width {
            case: "and so do two other digits",
            literal: "15",
            stored: "15",
            bytes: 2,
        },
        Width {
            case: "a whole number's trailing zeros pair off and fall away",
            literal: "100",
            stored: "100",
            bytes: 2,
        },
        Width {
            case: "a zero inside the lower pair is stored with it",
            literal: "1010",
            stored: "1010",
            bytes: 3,
        },
        Width {
            case: "a third digit starts a pair of its own",
            literal: "12345",
            stored: "12345",
            bytes: 4,
        },
        Width {
            case: "the same integral digits keep their zeros once a fraction follows",
            literal: "100.5",
            stored: "100.5",
            bytes: 4,
        },
        Width {
            case: "a fraction splits the pair the point falls in",
            literal: "1.5",
            stored: "1.5",
            bytes: 3,
        },
        Width {
            case: "an interior zero either side of the point",
            literal: "10.01",
            stored: "10.01",
            bytes: 3,
        },
        Width {
            case: "a fraction behind the zeros of a whole number",
            literal: "1010.5",
            stored: "1010.5",
            bytes: 4,
        },
        Width {
            case: "a two-digit fraction behind them",
            literal: "1200.75",
            stored: "1200.75",
            bytes: 4,
        },
        Width {
            case: "a fraction's own trailing zeros pair off and fall away",
            literal: "12.00",
            stored: "12",
            bytes: 2,
        },
        Width {
            case: "a leading pair of zeros is the exponent's business",
            literal: "0.00123",
            stored: "0.00123",
            bytes: 3,
        },
        Width {
            case: "an exponent states the zeros rather than storing them",
            literal: "1E+2",
            stored: "100",
            bytes: 2,
        },
        Width {
            case: "zero holds no pairs at all",
            literal: "0",
            stored: "0",
            bytes: 1,
        },
        Width {
            case: "a negative number pays one byte more than its magnitude",
            literal: "-100.5",
            stored: "-100.5",
            bytes: 5,
        },
        Width {
            // The documented precision is 38 digits ("Number: ... can have up to 38 digits
            // of precision" — "Service, Account, and Table Quotas"), which spans 19 pairs
            // when the point aligns it and 20 when it does not: 20 and 21 bytes, the
            // documented maximum width of an `N` value.
            case: "the precision ceiling, aligned on the point",
            literal: "12345678901234567890123456789012345678",
            stored: "12345678901234567890123456789012345678",
            bytes: 20,
        },
        Width {
            case: "the precision ceiling, split by the point",
            literal: "1.2345678901234567890123456789012345678",
            stored: "1.2345678901234567890123456789012345678",
            bytes: 21,
        },
    ]
}

/// The table every probe writes to.
const SIZED: &str = "NumberWidths";

/// Create that table and wait until it serves requests.
async fn table(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": SIZED,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        }),
    )
    .await;
    describe_when_active(router, "the table the width probes write to", SIZED).await;
}

/// How many bytes of an item of `bytes` metered bytes are left for the filler, once its
/// key, the number under test and every attribute NAME are paid for — an attribute costs
/// its name's bytes as well as its value's.
fn filler_for(key: &str, number_bytes: usize, bytes: usize) -> usize {
    bytes - ("pk".len() + key.len() + "n".len() + number_bytes + "f".len())
}

/// An item of exactly `bytes` metered bytes carrying `literal` as its own attribute.
fn item(key: &str, literal: &str, number_bytes: usize, bytes: usize) -> Value {
    json!({
        "pk": {"S": key},
        "n": {"N": literal},
        "f": {"S": "x".repeat(filler_for(key, number_bytes, bytes))},
    })
}

#[tokio::test]
async fn an_item_sized_to_the_ceiling_under_that_width_is_stored_and_reads_back_whole() {
    let router = emulator();
    table(&router).await;

    for (position, row) in widths().into_iter().enumerate() {
        let key = format!("k{position:02}");
        let case = row.case;
        let filler = filler_for(&key, row.bytes, MAX_ITEM_BYTES);

        // Exactly at the ceiling: stored.
        let (status, body) = call(
            &router,
            "PutItem",
            json!({
                "TableName": SIZED,
                "Item": item(&key, row.literal, row.bytes, MAX_ITEM_BYTES),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an item of exactly {MAX_ITEM_BYTES} bytes with {literal:?} \
             ({bytes} bytes wide) must be stored, got {status} with {body}",
            literal = row.literal,
            bytes = row.bytes
        );

        // ...and it reads back whole, the number in the one form the store holds it in.
        let read = ok(
            &router,
            "GetItem",
            json!({
                "TableName": SIZED,
                "Key": {"pk": {"S": key.clone()}},
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            (
                read["Item"]["n"]["N"].as_str(),
                read["Item"]["f"]["S"].as_str().map(str::len)
            ),
            (Some(row.stored), Some(filler)),
            "case {case:?}: the stored item must read back whole with {literal:?} as \
             {stored:?}, got {}",
            read["Item"]["n"],
            literal = row.literal,
            stored = row.stored
        );

        // One byte past the ceiling: refused with the sentence that quota states.
        let (status, body) = call(
            &router,
            "PutItem",
            json!({
                "TableName": SIZED,
                "Item": item(&key, row.literal, row.bytes, MAX_ITEM_BYTES + 1),
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", TOO_LARGE),
            "case {case:?}: one byte past the ceiling must be refused, got {status} with \
             {body}"
        );

        // ...and that refusal persisted nothing: the item at the ceiling is still the
        // stored one.
        let read = ok(
            &router,
            "GetItem",
            json!({
                "TableName": SIZED,
                "Key": {"pk": {"S": key.clone()}},
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            read["Item"]["f"]["S"].as_str().map(str::len),
            Some(filler),
            "case {case:?}: the refused write must leave the item at the ceiling behind, \
             got {}",
            read["Item"]["f"]
        );
    }
}

/// What `PutItem` reported it consumed.
async fn write_units(router: &MultiServiceRouter, case: &str, body: Value) -> f64 {
    let mut request = body;
    request["ReturnConsumedCapacity"] = json!("TOTAL");
    let answered = ok(router, "PutItem", request).await;
    answered["ConsumedCapacity"]["CapacityUnits"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: a write reports its capacity, got {answered}"))
}

#[tokio::test]
async fn the_same_width_is_what_the_capacity_units_are_counted_over() {
    let router = emulator();
    table(&router).await;

    for (position, row) in widths().into_iter().enumerate() {
        let case = row.case;

        // A write of an item sized exactly to one write unit costs that unit, and a write
        // of the same item one byte wider costs two.
        for (step, expected) in [(0usize, 1.0f64), (1, 2.0)] {
            let key = format!("w{position:02}{step}");
            let units = write_units(
                &router,
                case,
                json!({
                    "TableName": SIZED,
                    "Item": item(&key, row.literal, row.bytes, WRITE_UNIT_BYTES + step),
                }),
            )
            .await;
            assert_eq!(
                units, expected,
                "case {case:?}: a write of {} bytes with {literal:?} ({bytes} bytes wide) \
                 costs {expected} units, got {units}",
                WRITE_UNIT_BYTES + step,
                literal = row.literal,
                bytes = row.bytes
            );
        }

        // The same boundary for a strongly consistent read, at its own unit size.
        for (step, expected) in [(0usize, 1.0f64), (1, 2.0)] {
            let key = format!("r{position:02}{step}");
            ok(
                &router,
                "PutItem",
                json!({
                    "TableName": SIZED,
                    "Item": item(&key, row.literal, row.bytes, READ_UNIT_BYTES + step),
                }),
            )
            .await;
            let answered = ok(
                &router,
                "GetItem",
                json!({
                    "TableName": SIZED,
                    "Key": {"pk": {"S": key.clone()}},
                    "ConsistentRead": true,
                    "ReturnConsumedCapacity": "TOTAL",
                }),
            )
            .await;
            let units = answered["ConsumedCapacity"]["CapacityUnits"]
                .as_f64()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a read reports its capacity, got {answered}")
                });
            assert_eq!(
                units, expected,
                "case {case:?}: a strongly consistent read of {} bytes with {literal:?} \
                 ({bytes} bytes wide) costs {expected} units, got {units}",
                READ_UNIT_BYTES + step,
                literal = row.literal,
                bytes = row.bytes
            );
        }
    }
}

/// Where in an item the number under test sits, and what the container around it costs on
/// top of the number itself.
///
/// A list is "3 bytes of overhead, plus the size of each element" and one byte an element,
/// and a set is the sum of its members' sizes (the Developer Guide's "Item sizes and
/// formats") — the number's own width is the same in all three.
struct Placement {
    case: &'static str,
    overhead: usize,
    shape: fn(&str) -> Value,
}

fn placements() -> Vec<Placement> {
    vec![
        Placement {
            case: "a number of its own",
            overhead: 0,
            shape: |literal| json!({"N": literal}),
        },
        Placement {
            case: "a number inside a list",
            overhead: 4,
            shape: |literal| json!({"L": [{"N": literal}]}),
        },
        Placement {
            case: "a number inside a number set",
            overhead: 0,
            shape: |literal| json!({"NS": [literal]}),
        },
    ]
}

#[tokio::test]
async fn the_width_follows_the_value_wherever_in_the_item_it_sits() {
    let router = emulator();
    table(&router).await;

    // Three literals the alignment tells apart: trailing zeros that fall away, the same
    // digits with a fraction behind them, and a zero that is stored with its pair.
    let literals = ["100", "100.5", "1010"];
    for (at, literal) in literals.iter().enumerate() {
        let row = widths()
            .into_iter()
            .find(|row| row.literal == *literal)
            .expect("the catalogue carries every literal the placements use");
        for (position, placement) in placements().into_iter().enumerate() {
            let case = format!("{} holding {literal:?}", placement.case);
            for (step, expected) in [(0usize, 1.0f64), (1, 2.0)] {
                let key = format!("n{at}{position}{step}");
                let filler = WRITE_UNIT_BYTES + step
                    - ("pk".len()
                        + key.len()
                        + "n".len()
                        + row.bytes
                        + placement.overhead
                        + "f".len());
                let units = write_units(
                    &router,
                    &case,
                    json!({
                        "TableName": SIZED,
                        "Item": {
                            "pk": {"S": key.clone()},
                            "n": (placement.shape)(literal),
                            "f": {"S": "x".repeat(filler)},
                        },
                    }),
                )
                .await;
                assert_eq!(
                    units, expected,
                    "case {case:?}: a write of {} bytes with the number {bytes} bytes wide \
                     costs {expected} units, got {units}",
                    WRITE_UNIT_BYTES + step,
                    bytes = row.bytes
                );
            }
        }
    }
}
