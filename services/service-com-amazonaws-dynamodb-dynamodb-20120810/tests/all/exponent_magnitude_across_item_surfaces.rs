// SPDX-License-Identifier: CC0-1.0

//! Which defect an `N` text whose exponent leaves the documented range earns, read on the
//! item-API surfaces that carry a number inside a KEY, inside a legacy update and inside a
//! legacy filter.
//!
//! RULE under test: an exponent is read as the signed 64-bit integer a decimal exponent is
//! carried in, so the WIDTH the run of digits is written at never decides the defect on its
//! own: a magnitude that integer can state is held to the documented range ("Number: ...
//! can have up to 38 digits of precision. Exponent range is 10^-130 to 10^125" — "Service,
//! Account, and Table Quotas" in the Amazon DynamoDB Developer Guide) and refused as the
//! over- or underflow the SIGN of its exponent names, while a text whose exponent that
//! integer cannot state, or which is no signed run of digits at all, is refused as a
//! parameter that cannot be converted to a numeric value.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the WIDTH the exponent is written at — one past each limit of the 32-bit integer the
//!   statement surface reads one into, far past that width, AT each limit of the 64-bit one
//!   this surface reads one into and one past each of those, under an explicit sign, behind a
//!   long run of padding zeros, and (the controls) just outside each published bound of the
//!   documented range and just inside it;
//! * the DIRECTION the magnitude left the range in, which is what picks the sentence;
//! * the SURFACE the text arrives through — a key of a read, a key of a transactional write,
//!   an attribute of a legacy `AttributeUpdates` update, the operand of a legacy `QueryFilter`
//!   comparison and an element of a transactional read — because every item-API surface reads
//!   a number with the same reader.
//!
//! The sibling suites cover the rest of the same rule: `number_magnitude_refusal_surfaces`
//! the expression and batch readers, `exponent_width_conversion_refusals` the narrower reader
//! the PartiQL surfaces coerce a parameter through, and `attribute_value_admission_rules` a
//! written item attribute.
//!
//! Each surface states a clause about a value a request carried in its own envelope, so every
//! assertion is of the WHOLE message that surface states, and every row is driven over the
//! wire through the real router.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: a string partition key and a NUMBER sort key, so a number
/// arrives as a key as well as as an attribute.
const TABLE: &str = "ExponentMagnitudeItemSurfaces";

/// The partition every row below addresses.
const PARTITION: &str = "magnitude";

/// The envelope a clause about a value an item-API MEMBER carried is stated inside.
const ENVELOPE: &str = "1 validation error detected: ";

/// The two sentences a magnitude outside the documented range earns, either way out.
const OVERFLOW_CLAUSE: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";
const UNDERFLOW_CLAUSE: &str =
    "Number underflow. Attempting to store a number with magnitude smaller than supported range";

/// The sentence a text whose exponent is no run of digits at all earns, echoing the text.
fn conversion_clause(text: &str) -> String {
    format!("The parameter cannot be converted to a numeric value: {text}")
}

/// The published bounds of the documented exponent range, the two limits of the signed
/// 32-bit integer the statement surface reads an exponent into, and the two limits of the
/// signed 64-bit one every surface below reads one into.
const MAX_EXPONENT: i32 = 125;
const MIN_EXPONENT: i32 = -130;
const MAX_SIGNED_32: i64 = 2_147_483_647;
const MIN_SIGNED_32: i64 = -2_147_483_648;
const MAX_SIGNED_64: &str = "9223372036854775807";
const MIN_SIGNED_64: &str = "-9223372036854775808";

/// An emulator holding that one table with one item under the partition the rows read.
async fn seeded() -> MultiServiceRouter {
    let router = crate::emulator::emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "N")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": PARTITION}, "sk": {"N": "1"}}}),
    )
    .await;
    router
}

/// One row: how the exponent is written, and the whole clause the text earns.
struct Row {
    case: &'static str,
    text: String,
    clause: String,
}

/// The magnitudes, at every width the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one past the top of the integer the exponent is carried in",
            text: format!("1e{}", MAX_SIGNED_32 + 1),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "one past the bottom of that integer",
            text: format!("1e{}", MIN_SIGNED_32 - 1),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "at the top of that integer",
            text: format!("1e{MAX_SIGNED_32}"),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "at the bottom of that integer",
            text: format!("1e{MIN_SIGNED_32}"),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "far past the width of that integer",
            text: "1e99999999999".to_string(),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "far past that width, the other way",
            text: "1e-99999999999".to_string(),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "at the top of the integer this surface reads an exponent into",
            text: format!("1e{MAX_SIGNED_64}"),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "at the bottom of that integer",
            text: format!("1e{MIN_SIGNED_64}"),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "one past the top of that integer, which states no magnitude at all",
            text: "1e9223372036854775808".to_string(),
            clause: conversion_clause("1e9223372036854775808"),
        },
        Row {
            case: "one past the bottom of that integer",
            text: "1e-9223372036854775809".to_string(),
            clause: conversion_clause("1e-9223372036854775809"),
        },
        Row {
            case: "under an explicit plus sign on the mantissa and the exponent",
            text: "+1E+99999999999".to_string(),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "a fractional, negative mantissa under an exponent past that width",
            text: "-1.5e99999999999".to_string(),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "past the documented maximum behind a long run of padding zeros",
            text: format!("1e{}{}", "0".repeat(40), MAX_EXPONENT + 1),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "past the documented minimum behind a long run of padding zeros",
            text: format!("1e-{}{}", "0".repeat(40), (MIN_EXPONENT - 1).unsigned_abs()),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        // The controls at each published bound: one past it either way is the bound being
        // crossed, and the two texts whose exponent states no run of digits at all are the
        // ones this surface cannot convert.
        Row {
            case: "one past the documented maximum",
            text: format!("1e{}", MAX_EXPONENT + 1),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "one past the documented minimum",
            text: format!("1e{}", MIN_EXPONENT - 1),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "an exponent marker with no run of digits behind it",
            text: "1e".to_string(),
            clause: conversion_clause("1e"),
        },
        Row {
            case: "an exponent that is not digits at all",
            text: "1e12x".to_string(),
            clause: conversion_clause("1e12x"),
        },
    ]
}

/// One surface a number arrives through: the operation, the request built around the `N`
/// text, and whether that surface states its clause inside the per-member envelope.
struct Surface {
    case: &'static str,
    operation: &'static str,
    request: fn(&str) -> Value,
    enveloped: bool,
}

/// The table's key with `sk` carrying `text`.
fn key_with(text: &str) -> Value {
    json!({"pk": {"S": PARTITION}, "sk": {"N": text}})
}

/// The surfaces: a key of a read, a key and an attribute of a transactional write, a legacy
/// update, a legacy filter and a transactional read.
fn surfaces() -> Vec<Surface> {
    vec![
        Surface {
            case: "a key of GetItem",
            operation: "GetItem",
            request: |text| json!({"TableName": TABLE, "Key": key_with(text)}),
            enveloped: false,
        },
        Surface {
            case: "an AttributeUpdates value of UpdateItem",
            operation: "UpdateItem",
            request: |text| {
                json!({
                    "TableName": TABLE,
                    "Key": {"pk": {"S": PARTITION}, "sk": {"N": "1"}},
                    "AttributeUpdates": {
                        "v": {"Action": "PUT", "Value": {"N": text}},
                    },
                })
            },
            enveloped: true,
        },
        Surface {
            case: "a QueryFilter operand of Query",
            operation: "Query",
            request: |text| {
                json!({
                    "TableName": TABLE,
                    "KeyConditions": {
                        "pk": {
                            "ComparisonOperator": "EQ",
                            "AttributeValueList": [{"S": PARTITION}],
                        },
                    },
                    "QueryFilter": {
                        "v": {
                            "ComparisonOperator": "GT",
                            "AttributeValueList": [{"N": text}],
                        },
                    },
                })
            },
            enveloped: false,
        },
        Surface {
            case: "a sort-key condition value of Query",
            operation: "Query",
            request: |text| {
                json!({
                    "TableName": TABLE,
                    "KeyConditionExpression": "pk = :p AND sk = :s",
                    "ExpressionAttributeValues": {
                        ":p": {"S": PARTITION},
                        ":s": {"N": text},
                    },
                })
            },
            enveloped: false,
        },
        Surface {
            case: "a Delete key inside TransactWriteItems",
            operation: "TransactWriteItems",
            request: |text| {
                json!({"TransactItems": [
                    {"Delete": {"TableName": TABLE, "Key": key_with(text)}},
                ]})
            },
            enveloped: false,
        },
        Surface {
            case: "a Put attribute inside TransactWriteItems",
            operation: "TransactWriteItems",
            request: |text| {
                json!({"TransactItems": [
                    {"Put": {"TableName": TABLE, "Item": {
                        "pk": {"S": PARTITION}, "sk": {"N": "1"}, "v": {"N": text},
                    }}},
                ]})
            },
            enveloped: false,
        },
        Surface {
            case: "a Get key inside TransactGetItems",
            operation: "TransactGetItems",
            request: |text| {
                json!({"TransactItems": [
                    {"Get": {"TableName": TABLE, "Key": key_with(text)}},
                ]})
            },
            enveloped: false,
        },
    ]
}

#[tokio::test]
async fn the_width_of_the_integer_an_exponent_is_read_into_decides_the_defect() {
    let router = seeded().await;

    for surface in surfaces() {
        for row in rows() {
            let case = format!("{} carrying a text {}", surface.case, row.case);
            let (status, answered) =
                call(&router, surface.operation, (surface.request)(&row.text)).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "ValidationException"),
                "case {case:?}: the text is refused, got {status} with {answered}"
            );
            let expected = if surface.enveloped {
                format!("{ENVELOPE}{}", row.clause)
            } else {
                row.clause.clone()
            };
            let stated = message(&answered);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?} with {answered}"
            );
        }
    }
}

#[tokio::test]
async fn each_published_bound_is_still_served_on_every_surface() {
    let router = seeded().await;

    // The control: a magnitude ON each published bound is served on every surface above, so
    // the refusals are the bound being crossed and not a refusal of every exponent. The
    // write surfaces leave the seeded item addressable either way, since each addresses the
    // partition's own key.
    for surface in surfaces() {
        for (bound, text) in [
            ("the documented maximum exponent", format!("1e{MAX_EXPONENT}")),
            ("the documented minimum exponent", format!("1e{MIN_EXPONENT}")),
        ] {
            let case = format!("{} at {bound}", surface.case);
            let (status, answered) = call(&router, surface.operation, (surface.request)(&text)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a magnitude on the published bound is served, got {status} \
                 with {answered}"
            );
        }
    }
}

#[tokio::test]
async fn a_refused_write_leaves_the_stored_item_where_it_was() {
    let router = seeded().await;

    // A refusal is about the value the request carried, so nothing it named is written and
    // nothing it addressed is removed: the seeded item is still readable after every row of
    // every write surface has been refused.
    for surface in surfaces() {
        for row in rows() {
            let answered = call(&router, surface.operation, (surface.request)(&row.text)).await;
            assert_eq!(
                answered.0, 400,
                "case {}/{}: expected the refusal, got {answered:?}",
                surface.case, row.case
            );
        }
    }
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": PARTITION}, "sk": {"N": "1"}}}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"pk": {"S": PARTITION}, "sk": {"N": "1"}}),
        "case \"after every refusal\": expected the seeded item whole, got {read}"
    );
}
