// SPDX-License-Identifier: CC0-1.0

//! What an `N` text whose EXPONENT is written at a great WIDTH earns, on every surface a
//! number arrives through.
//!
//! RULE under test: an exponent is read as the signed integer a decimal exponent is carried
//! in, and a magnitude that integer CAN state is then held to the documented range ("Number:
//! ... can have up to 38 digits of precision. Exponent range is 10^-130 to 10^125" —
//! "Service, Account, and Table Quotas" in the Amazon DynamoDB Developer Guide) and refused
//! as the over- or underflow the direction it left that range in names; only a text whose
//! exponent that integer cannot state — or which is no signed run of digits at all — cannot
//! be converted to a numeric value. The WIDTH of that integer is the surface's own: the item
//! API reads an exponent into a 64-bit one, and the STATEMENT surface coerces a `Parameters`
//! entry through a narrower 32-bit reader, so one text can be a magnitude to the first and no
//! number at all to the second.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the WIDTH the exponent is written at — one past each limit of the 32-bit reader, far past
//!   it either way, far past it with an explicit sign, wider than any integer, and (the
//!   controls) exactly at each 32-bit limit, just outside the documented range, and inside
//!   it;
//! * the SIGN of the exponent, which is what decides which sentence a magnitude earns;
//! * the SURFACE the text arrives through: an item attribute of a write, a key of a read, an
//!   expression value, a PartiQL parameter of the single-statement surface, of a transaction
//!   and of a batch — which is what decides WHICH reader the text is read with.
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

/// The sentence a text whose exponent is no run of digits at all earns, echoing the text as
/// sent.
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

/// One row: the case, the `N` text, the clause an ITEM-API surface earns for it and the
/// clause a STATEMENT surface earns for it. The two differ only where the text's exponent
/// lies between the two readers' widths.
struct Row {
    case: &'static str,
    text: String,
    clause: String,
    statement_clause: String,
}

/// The texts whose exponent is written wider than the 32-bit reader of the statement surface,
/// the controls just inside that width, and the texts whose exponent is no run of digits at
/// all.
fn rows() -> Vec<Row> {
    /// One row whose text is past the STATEMENT reader's width: a magnitude to the item API,
    /// and a parameter the statement surface cannot convert.
    fn past_the_statement_reader(case: &'static str, text: String, clause: &str) -> Row {
        Row {
            case,
            statement_clause: conversion_clause(&text),
            text,
            clause: clause.to_string(),
        }
    }
    /// One row both readers answer the same way.
    fn read_alike(case: &'static str, text: String, clause: String) -> Row {
        Row {
            case,
            text,
            clause: clause.clone(),
            statement_clause: clause,
        }
    }
    let mut rows = vec![
        past_the_statement_reader(
            "one past the top of the 32-bit reader",
            format!("1e{}", MAX_SIGNED_32 + 1),
            OVERFLOW_CLAUSE,
        ),
        past_the_statement_reader(
            "one past the bottom of that reader",
            format!("1e{}", MIN_SIGNED_32 - 1),
            UNDERFLOW_CLAUSE,
        ),
        past_the_statement_reader(
            "far past the top of that reader",
            "1e99999999999".to_string(),
            OVERFLOW_CLAUSE,
        ),
        past_the_statement_reader(
            "far past the bottom of that reader",
            "1e-99999999999".to_string(),
            UNDERFLOW_CLAUSE,
        ),
        past_the_statement_reader(
            "far past it with an explicit plus sign on the exponent",
            "1E+99999999999".to_string(),
            OVERFLOW_CLAUSE,
        ),
        past_the_statement_reader(
            "a fractional mantissa under an exponent past that width",
            "-1.5e99999999999".to_string(),
            OVERFLOW_CLAUSE,
        ),
        // Wider than EITHER reader, so neither can state the magnitude and both answer the
        // text they cannot convert.
        read_alike(
            "an exponent far wider than any integer, written out",
            "1E99999999999999999999999999999999999999999".to_string(),
            conversion_clause("1E99999999999999999999999999999999999999999"),
        ),
        // The controls: AT each limit of the narrower reader both read a magnitude, and so
        // they do just outside the documented range.
        read_alike(
            "at the top of that reader, which names a magnitude",
            format!("1e{MAX_SIGNED_32}"),
            OVERFLOW_CLAUSE.to_string(),
        ),
        read_alike(
            "at the bottom of that reader, which names a magnitude",
            format!("1e{MIN_SIGNED_32}"),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        read_alike(
            "one past the documented maximum",
            format!("1e{}", MAX_EXPONENT + 1),
            OVERFLOW_CLAUSE.to_string(),
        ),
        read_alike(
            "one past the documented minimum",
            format!("1e{}", MIN_EXPONENT - 1),
            UNDERFLOW_CLAUSE.to_string(),
        ),
        // The control for the OTHER refusal: an exponent marker with no run of digits behind
        // it states no exponent to either reader.
        read_alike(
            "an exponent marker with no digits behind it",
            "1e".to_string(),
            conversion_clause("1e"),
        ),
        read_alike(
            "an exponent that is not digits at all",
            "1e4x".to_string(),
            conversion_clause("1e4x"),
        ),
    ];
    // Leading zeros denote nothing, so a padded exponent is judged by the magnitude behind
    // the padding: this one is inside both readers' widths and outside the documented range.
    rows.push(read_alike(
        "past the documented maximum behind a long run of leading zeros",
        format!("1e{}{}", "0".repeat(30), MAX_EXPONENT + 1),
        OVERFLOW_CLAUSE.to_string(),
    ));
    rows
}

/// One surface a number arrives through: the operation, the request built around the text,
/// whether that surface states its clause inside the per-member validation envelope, and
/// whether it coerces the text through the STATEMENT reader.
struct Surface {
    case: &'static str,
    operation: &'static str,
    request: fn(&str) -> Value,
    enveloped: bool,
    statement: bool,
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
            statement: false,
        },
        Surface {
            case: "a key of GetItem",
            operation: "GetItem",
            request: |text| {
                json!({"TableName": TABLE, "Key": {"pk": {"S": "exp"}, "sk": {"N": text}}})
            },
            enveloped: false,
            statement: false,
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
            statement: false,
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
            statement: true,
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
            statement: true,
        },
    ]
}

#[tokio::test]
async fn which_reader_a_surface_uses_decides_which_defect_a_wide_exponent_is() {
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
            let clause = if surface.statement {
                &row.statement_clause
            } else {
                &row.clause
            };
            let expected = if surface.enveloped {
                format!("{ENVELOPE}{clause}")
            } else {
                clause.clone()
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
    // states the clause behind the slot that carried the parameter — the STATEMENT reader's,
    // since a batch slot carries a `Parameters` entry.
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
            (Some("ValidationError"), Some(row.statement_clause.as_str())),
            "case {:?}: the slot states {:?}, got {answered}",
            row.case,
            row.statement_clause
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
