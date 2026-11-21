// SPDX-License-Identifier: CC0-1.0

//! What an `N` text whose EXPONENT is wider than a decimal exponent can be earns, on every
//! surface a number arrives through.
//!
//! RULE under test: a decimal's exponent is held in a signed 32-bit integer, so a text whose
//! exponent states a magnitude outside that integer states no decimal number at all and is
//! refused as a parameter that cannot be converted to a numeric value, echoing the text; a
//! text whose exponent that integer CAN state does name a number, and only then is its
//! magnitude held to the documented range ("Number: ... can have up to 38 digits of
//! precision. Exponent range is 10^-130 to 10^125" — "Service, Account, and Table Quotas" in
//! the Amazon DynamoDB Developer Guide) as the over- or underflow it is.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the WIDTH the exponent is written at — one past each limit of that integer, far past it
//!   either way, far past it with an explicit sign, and (the controls) exactly at each limit,
//!   just outside the documented range, and inside it;
//! * the SIGN of the exponent, which decides nothing about this refusal and everything about
//!   the magnitude ones;
//! * the SURFACE the text arrives through: an item attribute of a write, a key of a read, an
//!   expression value, a PartiQL parameter of the single-statement surface, of a transaction
//!   and of a batch — because the rule belongs to the numeric domain and not to one
//!   operation.
//!
//! Each surface states a clause about a value a request carried in its own envelope, so each
//! assertion is of the WHOLE message that surface states, and the batch slot's own
//! `Error.Code`/`Error.Message` pair is asserted where the batch reports it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row below addresses: a string partition key and a NUMBER sort key, so a
/// number can arrive as a key as well as as an attribute.
const TABLE: &str = "ExponentWidthSurfaces";

/// The envelope a clause about a value an item-API MEMBER carried is stated inside.
const ENVELOPE: &str = "1 validation error detected: ";

/// The two sentences a magnitude a decimal CAN state and the documented range excludes earns.
const OVERFLOW_CLAUSE: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";
const UNDERFLOW_CLAUSE: &str =
    "Number underflow. Attempting to store a number with magnitude smaller than supported range";

/// The sentence a text that states no decimal number earns, echoing the text as sent.
fn conversion_clause(text: &str) -> String {
    format!("The parameter cannot be converted to a numeric value: {text}")
}

/// The limits of the signed 32-bit integer a decimal exponent is held in, and the published
/// bounds of the documented exponent range, as a text spells them.
const MAX_SIGNED_32: i64 = 2_147_483_647;
const MIN_SIGNED_32: i64 = -2_147_483_648;
const MAX_EXPONENT: i32 = 125;
const MIN_EXPONENT: i32 = -130;

/// An emulator holding that one table, with one item under the key the rows read.
async fn seeded() -> MultiServiceRouter {
    let router = emulator_with_table().await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "exp"}, "sk": {"N": "1"}}}),
    )
    .await;
    router
}

/// The emulator with the table alone.
async fn emulator_with_table() -> MultiServiceRouter {
    let router = crate::emulator::emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "N")]).await;
    router
}

/// One row: the case, the `N` text, and the whole clause it earns.
struct Row {
    case: &'static str,
    text: String,
    clause: String,
}

/// The texts whose exponent is wider than a decimal exponent can be, and the controls just
/// inside that width.
fn rows() -> Vec<Row> {
    let mut rows = vec![
        Row {
            case: "one past the top of the integer the exponent is held in",
            text: format!("1e{}", MAX_SIGNED_32 + 1),
            clause: conversion_clause(&format!("1e{}", MAX_SIGNED_32 + 1)),
        },
        Row {
            case: "one past the bottom of that integer",
            text: format!("1e{}", MIN_SIGNED_32 - 1),
            clause: conversion_clause(&format!("1e{}", MIN_SIGNED_32 - 1)),
        },
        Row {
            case: "far past the top of that integer",
            text: "1e99999999999".to_string(),
            clause: conversion_clause("1e99999999999"),
        },
        Row {
            case: "far past the bottom of that integer",
            text: "1e-99999999999".to_string(),
            clause: conversion_clause("1e-99999999999"),
        },
        Row {
            case: "far past it with an explicit plus sign on the exponent",
            text: "1E+99999999999".to_string(),
            clause: conversion_clause("1E+99999999999"),
        },
        Row {
            case: "an exponent far wider than any integer, written out",
            text: "1E99999999999999999999999999999999999999999".to_string(),
            clause: conversion_clause("1E99999999999999999999999999999999999999999"),
        },
        Row {
            case: "a fractional mantissa under an exponent past that width",
            text: "-1.5e99999999999".to_string(),
            clause: conversion_clause("-1.5e99999999999"),
        },
        // The controls: AT each limit of that integer the text DOES name a number, whose
        // magnitude the documented range then excludes.
        Row {
            case: "at the top of that integer, which names a magnitude",
            text: format!("1e{MAX_SIGNED_32}"),
            clause: OVERFLOW_CLAUSE.to_string(),
        },
        Row {
            case: "at the bottom of that integer, which names a magnitude",
            text: format!("1e{MIN_SIGNED_32}"),
            clause: UNDERFLOW_CLAUSE.to_string(),
        },
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
    ];
    // Leading zeros denote nothing, so a padded exponent is judged by the magnitude behind
    // the padding: this one is inside the width and outside the documented range.
    rows.push(Row {
        case: "past the documented maximum behind a long run of leading zeros",
        text: format!("1e{}{}", "0".repeat(30), MAX_EXPONENT + 1),
        clause: OVERFLOW_CLAUSE.to_string(),
    });
    rows
}

/// One surface a number arrives through: the operation, the request built around the text,
/// and whether that surface states its clause inside the per-member validation envelope.
struct Surface {
    case: &'static str,
    operation: &'static str,
    request: fn(&str) -> Value,
    enveloped: bool,
}

/// The surfaces: the item API, the expression surface and the three PartiQL ones.
fn surfaces() -> Vec<Surface> {
    vec![
        Surface {
            case: "an item attribute of PutItem",
            operation: "PutItem",
            request: |text| {
                json!({"TableName": TABLE, "Item": {
                    "pk": {"S": "exp"}, "sk": {"N": "1"}, "v": {"N": text},
                }})
            },
            enveloped: true,
        },
        Surface {
            case: "a key of GetItem",
            operation: "GetItem",
            request: |text| {
                json!({"TableName": TABLE, "Key": {"pk": {"S": "exp"}, "sk": {"N": text}}})
            },
            enveloped: false,
        },
        Surface {
            case: "an expression value of a Query key condition",
            operation: "Query",
            request: |text| {
                json!({
                    "TableName": TABLE,
                    "KeyConditionExpression": "pk = :p AND sk = :s",
                    "ExpressionAttributeValues": {":p": {"S": "exp"}, ":s": {"N": text}},
                })
            },
            enveloped: false,
        },
        Surface {
            case: "a parameter of ExecuteStatement",
            operation: "ExecuteStatement",
            request: |text| {
                json!({
                    "Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'exp' AND sk = ?"),
                    "Parameters": [{"N": text}],
                })
            },
            enveloped: false,
        },
        Surface {
            case: "a parameter of ExecuteTransaction",
            operation: "ExecuteTransaction",
            request: |text| {
                json!({"TransactStatements": [{
                    "Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'exp' AND sk = ?"),
                    "Parameters": [{"N": text}],
                }]})
            },
            enveloped: false,
        },
    ]
}

#[tokio::test]
async fn an_exponent_wider_than_a_decimal_carries_cannot_be_converted_on_every_surface() {
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
async fn a_batch_slot_states_the_same_clause_in_its_own_error() {
    let router = seeded().await;

    // "The Error output member represents the error, if any, that was encountered while
    // processing the request" (`BatchStatementResponse$Error`): the batch serves the call and
    // states the clause behind the slot that carried the parameter.
    for row in rows() {
        let answered = ok(
            &router,
            "BatchExecuteStatement",
            json!({"Statements": [
                {"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'exp' AND sk = ?"),
                 "Parameters": [{"N": "1"}]},
                {"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = 'exp' AND sk = ?"),
                 "Parameters": [{"N": row.text}]},
            ]}),
        )
        .await;
        assert_eq!(
            (
                answered["Responses"][1]["Error"]["Code"].as_str(),
                answered["Responses"][1]["Error"]["Message"].as_str(),
            ),
            (Some("ValidationError"), Some(row.clause.as_str())),
            "case {:?}: the slot states {:?}, got {answered}",
            row.case,
            row.clause
        );
        // The slot that carried a well-formed parameter is served, so the batch reports the
        // refusal of one statement and not of the request.
        assert!(
            answered["Responses"][0].get("Error").is_none(),
            "case {:?}: the well-formed slot is served, got {answered}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_exponent_inside_the_documented_range_is_still_served() {
    let router = emulator_with_table().await;

    // The control: a text whose exponent names a magnitude inside the documented range is
    // stored and read back as the one canonical decimal text that value has — "Numbers are
    // sent across the network to DynamoDB as strings" (`AttributeValue$N`) — so the rows
    // above are a bound being crossed and not a refusal of every exponent.
    for (case, text, canonical) in [
        (
            "the documented maximum exponent",
            "1e125".to_string(),
            format!("1{}", "0".repeat(125)),
        ),
        (
            "the documented minimum exponent",
            "1e-130".to_string(),
            format!("0.{}1", "0".repeat(129)),
        ),
        (
            "an exponent well inside the range",
            "15e2".to_string(),
            "1500".to_string(),
        ),
        (
            "the documented maximum behind a long run of leading zeros",
            format!("1e{}125", "0".repeat(30)),
            format!("1{}", "0".repeat(125)),
        ),
    ] {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {
                "pk": {"S": case}, "sk": {"N": "1"}, "v": {"N": text},
            }}),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": case}, "sk": {"N": "1"}}}),
        )
        .await;
        assert_eq!(
            read["Item"]["v"]["N"].as_str(),
            Some(canonical.as_str()),
            "case {case:?} ({text}): expected the stored number to read back as \
             {canonical:?}, got {read}"
        );
    }
}
