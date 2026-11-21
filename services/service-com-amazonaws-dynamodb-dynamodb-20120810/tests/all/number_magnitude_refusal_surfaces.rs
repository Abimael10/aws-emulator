// SPDX-License-Identifier: CC0-1.0

//! The refusals a NUMBER's magnitude earns, on every surface a number arrives through.
//!
//! The rule: a magnitude a decimal CAN state and the documented exponent range excludes is
//! refused as the over- or underflow it is, by the sign of the exponent ("Number: ... can
//! have up to 38 digits of precision. Exponent range is 10^-130 to 10^125" — "Service,
//! Account, and Table Quotas" in the Amazon DynamoDB Developer Guide), while a text whose
//! exponent states a magnitude wider than the signed 32-bit integer a decimal exponent is
//! held in denotes no decimal number at all and is refused as a text that cannot be
//! converted to a numeric value, echoing the text.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the SURFACE the number arrives through — an item attribute of a write, a key of a read
//!   or a write, an expression value of a query, and the same three inside a transaction or
//!   a batch — because the rule is the numeric domain's and not one operation's;
//! * the WIDTH the exponent is written at: just past a published bound, at the limits of the
//!   32-bit integer a decimal exponent is held in, one past and far past those limits in
//!   either direction, with an explicit `+` sign, and padded with a long run of leading
//!   zeros;
//! * the DIRECTION it left the range in, which is what decides which sentence is stated.
//!
//! Which ENVELOPE a surface states its clause in is that surface's own — a clause about a
//! value an input MEMBER carried is stated inside `1 validation error detected: `, one about
//! a value an expression, a transaction slot or a batch slot carried on its own — so each
//! row carries the envelope its surface uses and the whole message is asserted exactly.
//!
//! Every call goes over the wire through the real router, and each row's control — the
//! published bound itself — is still served, so nothing here passes by refusing everything.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below addresses: a string partition key and a NUMBER sort key, so a
/// number can arrive as a key as well as as an attribute.
const TABLE: &str = "NumberMagnitudeSurfaces";

/// The envelope a clause about a value an item-API MEMBER carried is stated inside; a clause
/// about a value an expression or a transaction slot carried is stated on its own.
const ENVELOPE: &str = "1 validation error detected: ";

/// The two sentences a magnitude outside the documented range earns, either way out.
const OVERFLOW_CLAUSE: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";
const UNDERFLOW_CLAUSE: &str =
    "Number underflow. Attempting to store a number with magnitude smaller than supported range";

/// The sentence a text that states no decimal number at all earns, echoing the text.
fn conversion_clause(text: &str) -> String {
    format!("The parameter cannot be converted to a numeric value: {text}")
}

/// The published bounds of the documented exponent range, and the two limits of the 32-bit
/// integer a decimal exponent is held in, as a text spells them.
const MAX_EXPONENT: i32 = 125;
const MIN_EXPONENT: i32 = -130;
const MAX_SIGNED_32: &str = "2147483647";
const MIN_SIGNED_32: &str = "-2147483648";

/// An emulator holding one active table.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the number-magnitude table", TABLE).await;
    router
}

/// One magnitude row: how it is written and which sentence it earns.
struct Magnitude {
    case: &'static str,
    text: String,
    clause: String,
}

/// The magnitudes outside the documented range, written at every width the rule
/// generalises over.
fn magnitudes() -> Vec<Magnitude> {
    let mut rows = vec![
        Magnitude {
            case: "one past the documented maximum",
            text: format!("1e{}", MAX_EXPONENT + 1),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Magnitude {
            case: "one past the documented minimum",
            text: format!("1e{}", MIN_EXPONENT - 1),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        Magnitude {
            case: "at the top of the integer the exponent is held in",
            text: format!("1e{MAX_SIGNED_32}"),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Magnitude {
            case: "at the bottom of that integer",
            text: format!("1e{MIN_SIGNED_32}"),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
        // Past the width of that integer the text states no decimal number at all, so it
        // is refused as one that cannot be converted, echoed as the request wrote it.
        Magnitude {
            case: "one past the top of that integer",
            text: "1e2147483648".to_string(),
            clause: conversion_clause("1e2147483648"),
        },
        Magnitude {
            case: "one past the bottom of that integer",
            text: "1e-2147483649".to_string(),
            clause: conversion_clause("1e-2147483649"),
        },
        Magnitude {
            case: "far past the width of that integer",
            text: "1e99999999999".to_string(),
            clause: conversion_clause("1e99999999999"),
        },
        Magnitude {
            case: "far past that width, the other way",
            text: "1e-99999999999".to_string(),
            clause: conversion_clause("1e-99999999999"),
        },
        Magnitude {
            case: "far past that width with an explicit plus sign",
            text: "+1e99999999999".to_string(),
            clause: conversion_clause("+1e99999999999"),
        },
    ];
    // The padding denotes nothing, so what decides the refusal is the magnitude behind it.
    rows.push(Magnitude {
        case: "past the maximum behind a long run of leading zeros",
        text: format!("1e{}{}", "0".repeat(40), MAX_EXPONENT + 1),
        clause: OVERFLOW_CLAUSE.to_string(),
    });
    rows.push(Magnitude {
        case: "past the minimum behind a long run of leading zeros",
        text: format!("1e-{}{}", "0".repeat(40), (MIN_EXPONENT - 1).unsigned_abs()),
        clause: UNDERFLOW_CLAUSE.to_string(),
    });
    rows
}

/// A magnitude INSIDE the documented range, at each published bound: the control every
/// surface is also driven with.
fn inside() -> [(&'static str, String); 2] {
    [
        ("the documented maximum", format!("1e{MAX_EXPONENT}")),
        ("the documented minimum", format!("1e{MIN_EXPONENT}")),
    ]
}

/// One surface a number arrives through: the operation, the request built around the
/// `N` text, and whether that surface states a clause inside the per-member validation
/// envelope (a value an item-API MEMBER carried) or on its own (a value an expression or a
/// transaction slot carried).
struct Surface {
    case: &'static str,
    operation: &'static str,
    request: fn(&str) -> Value,
    enveloped: bool,
}

/// A key naming the table's two key attributes, with `sk` carrying `text`.
fn key_with(text: &str) -> Value {
    json!({"pk": {"S": "magnitude"}, "sk": {"N": text}})
}

/// An item whose `v` attribute carries `text`, under a well-formed key.
fn item_with(text: &str) -> Value {
    json!({"pk": {"S": "magnitude"}, "sk": {"N": "1"}, "v": {"N": text}})
}

/// The surfaces: an attribute of a write, a key of a read and of a write, an expression
/// value of a query, and the transaction and batch forms of the same three.
fn surfaces() -> Vec<Surface> {
    vec![
        Surface {
            case: "an item attribute of PutItem",
            operation: "PutItem",
            request: |text| json!({"TableName": TABLE, "Item": item_with(text)}),
            enveloped: true,
        },
        Surface {
            case: "a key of GetItem",
            operation: "GetItem",
            request: |text| json!({"TableName": TABLE, "Key": key_with(text)}),
            enveloped: false,
        },
        Surface {
            case: "a key of DeleteItem",
            operation: "DeleteItem",
            request: |text| json!({"TableName": TABLE, "Key": key_with(text)}),
            enveloped: true,
        },
        Surface {
            case: "a key of UpdateItem",
            operation: "UpdateItem",
            request: |text| {
                json!({
                    "TableName": TABLE,
                    "Key": key_with(text),
                    "UpdateExpression": "SET v = :v",
                    "ExpressionAttributeValues": {":v": {"S": "updated"}},
                })
            },
            enveloped: true,
        },
        Surface {
            case: "an expression value of a Query key condition",
            operation: "Query",
            request: |text| {
                json!({
                    "TableName": TABLE,
                    "KeyConditionExpression": "pk = :p AND sk = :s",
                    "ExpressionAttributeValues": {
                        ":p": {"S": "magnitude"},
                        ":s": {"N": text},
                    },
                })
            },
            enveloped: false,
        },
        Surface {
            case: "an item attribute inside TransactWriteItems",
            operation: "TransactWriteItems",
            request: |text| {
                json!({
                    "TransactItems": [
                        {"Put": {"TableName": TABLE, "Item": item_with(text)}}
                    ]
                })
            },
            enveloped: false,
        },
        Surface {
            case: "a key inside TransactGetItems",
            operation: "TransactGetItems",
            request: |text| {
                json!({
                    "TransactItems": [
                        {"Get": {"TableName": TABLE, "Key": key_with(text)}}
                    ]
                })
            },
            enveloped: false,
        },
        Surface {
            case: "a key inside BatchGetItem",
            operation: "BatchGetItem",
            request: |text| {
                json!({"RequestItems": {TABLE: {"Keys": [key_with(text)]}}})
            },
            enveloped: false,
        },
    ]
}

#[tokio::test]
async fn a_magnitude_outside_the_documented_range_states_which_way_it_left_it() {
    let router = seeded().await;

    for surface in surfaces() {
        for magnitude in magnitudes() {
            let case = format!("{} with a magnitude {}", surface.case, magnitude.case);
            let (status, answered) =
                call(&router, surface.operation, (surface.request)(&magnitude.text)).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "ValidationException"),
                "case {case:?}: a magnitude outside the documented range is refused, got \
                 {status} with {answered}"
            );
            let stated = message(&answered);
            // The sentence is the clause itself, in the envelope this surface states a
            // clause in.
            let expected = if surface.enveloped {
                format!("{ENVELOPE}{}", magnitude.clause)
            } else {
                magnitude.clause.clone()
            };
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?} with {answered}"
            );
        }
    }
}

#[tokio::test]
async fn every_published_bound_is_still_served_on_every_surface() {
    let router = seeded().await;

    for surface in surfaces() {
        for (bound, text) in inside() {
            let case = format!("{} at {bound}", surface.case);
            let (status, answered) =
                call(&router, surface.operation, (surface.request)(&text)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a magnitude on the published bound is served, got {status} \
                 with {answered}"
            );
        }
    }
}
