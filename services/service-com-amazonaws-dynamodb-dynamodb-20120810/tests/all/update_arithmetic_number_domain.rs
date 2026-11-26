// SPDX-License-Identifier: CC0-1.0

//! The number an UPDATE COMPUTES is held to the same numeric domain every stored `N` is
//! held to.
//!
//! RULE: a `Number` carries at most 38 significant digits with its magnitude inside the
//! documented exponent range ("Number: ... can have up to 38 digits of precision. Exponent
//! range is 10^-130 to 10^125" — Service, Account, and Table Quotas), and the result of a
//! `SET` addition or subtraction, or of an `ADD`, is a number this store has to hold. A
//! result outside that domain is therefore refused 400 `ValidationException` with AWS's
//! numeric-overflow sentence and the item is left exactly as it was — never rounded to fit,
//! because a rounded result is a value the operands do not add up to and one a later read
//! against AWS would not report.
//!
//! The clause is stated on its OWN, out of the `<N> validation error(s) detected: `
//! envelope: that envelope reports a constraint on a value an input member carried, and no
//! member carried this number — the expression asked for it.
//!
//! The rows below span what the rule generalises over: which BOUND the result crosses (the
//! significant digits, the magnitude), the direction of the arithmetic (`+`, `-`), the
//! operator that computes it (`SET`'s arithmetic, `ADD`), where the result is written, and
//! each bound's own boundary — the largest result the domain holds, and the first one past
//! it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "ComputedNumbers";

/// AWS's fixed sentence for a computed number the numeric domain cannot hold.
const OVERFLOW: &str =
    "Number overflow. Attempting to store a number with magnitude larger than supported range";

/// The envelope a refusal about a value an input MEMBER carried is stated in, which a
/// computed number's refusal is not.
const ENVELOPE: &str = "1 validation error detected: ";

/// How many significant digits a stored number may carry (the quota above).
const DIGITS: usize = 38;

/// The largest exponent the documented range admits, as the quota states it.
const TOP_EXPONENT: usize = 125;

/// One row: what the item holds, the update that computes a new value from it, and what the
/// store is left with.
struct Row {
    case: &'static str,
    /// The `N` text the item carries before the update, in the canonical form a read
    /// reports it in, so a row that is refused can assert the item is untouched.
    stored: String,
    /// The `UpdateExpression` the row drives.
    expression: &'static str,
    /// The attribute it writes.
    target: &'static str,
    /// The `:v` operand it binds.
    operand: String,
    /// The canonical text `target` must hold afterwards, or `None` for a row the domain
    /// refuses — which must leave the item exactly as it was.
    computed: Option<String>,
}

/// The key one row works under, so the rows cannot disturb each other.
fn key(at: usize) -> Value {
    json!({"pk": {"S": format!("row-{at}")}})
}

fn rows() -> Vec<Row> {
    let nines = "9".repeat(DIGITS);
    vec![
        // The significant-digit bound, at its boundary: 38 nines plus one carries into a
        // single significant digit and is stored, while plus two needs 39 of them.
        Row {
            case: "38 nines plus one carries into one significant digit",
            stored: nines.clone(),
            expression: "SET n = n + :v",
            target: "n",
            operand: "1".to_string(),
            computed: Some(format!("1{}", "0".repeat(DIGITS))),
        },
        Row {
            case: "38 nines plus two would need 39 significant digits",
            stored: nines.clone(),
            expression: "SET n = n + :v",
            target: "n",
            operand: "2".to_string(),
            computed: None,
        },
        // The same bound reached by SUBTRACTION, its boundary one decimal apart.
        Row {
            case: "one minus the 38th decimal keeps 38 significant digits",
            stored: "1".to_string(),
            expression: "SET n = n - :v",
            target: "n",
            operand: format!("1e-{DIGITS}"),
            computed: Some(format!("0.{}", "9".repeat(DIGITS))),
        },
        Row {
            case: "one minus the 39th decimal would need 39 significant digits",
            stored: "1".to_string(),
            expression: "SET n = n - :v",
            target: "n",
            operand: format!("1e-{}", DIGITS + 1),
            computed: None,
        },
        // The MAGNITUDE bound, at its boundary: a sum at the top of the documented
        // exponent range is stored, one past it is not.
        Row {
            case: "a sum inside the documented exponent range",
            stored: format!("1{}", "0".repeat(TOP_EXPONENT)),
            expression: "SET n = n + :v",
            target: "n",
            operand: format!("1e{TOP_EXPONENT}"),
            computed: Some(format!("2{}", "0".repeat(TOP_EXPONENT))),
        },
        Row {
            case: "a sum past the documented exponent range",
            stored: format!("9{}", "0".repeat(TOP_EXPONENT)),
            expression: "SET n = n + :v",
            target: "n",
            operand: format!("2e{TOP_EXPONENT}"),
            computed: None,
        },
        // A subtraction well inside both bounds, so the rows above refuse for the bound
        // they cross and not for computing at all.
        Row {
            case: "a subtraction inside both bounds",
            stored: format!("1{}", "0".repeat(TOP_EXPONENT)),
            expression: "SET n = n - :v",
            target: "n",
            operand: format!("1e{}", TOP_EXPONENT - 1),
            computed: Some(format!("9{}", "0".repeat(TOP_EXPONENT - 1))),
        },
        // `ADD` computes the same sum, so it is held to the same domain.
        Row {
            case: "ADD inside the significant-digit bound",
            stored: nines.clone(),
            expression: "ADD n :v",
            target: "n",
            operand: "1".to_string(),
            computed: Some(format!("1{}", "0".repeat(DIGITS))),
        },
        Row {
            case: "ADD past the significant-digit bound",
            stored: nines.clone(),
            expression: "ADD n :v",
            target: "n",
            operand: "2".to_string(),
            computed: None,
        },
        // The target of the assignment is not what breaks the rule: the RESULT is, so an
        // assignment into another attribute is refused the same way.
        Row {
            case: "a sum past the bound assigned to another attribute",
            stored: nines.clone(),
            expression: "SET tally = n + :v",
            target: "tally",
            operand: "2".to_string(),
            computed: None,
        },
        Row {
            case: "a sum inside the bound assigned to another attribute",
            stored: nines.clone(),
            expression: "SET tally = n + :v",
            target: "tally",
            operand: "1".to_string(),
            computed: Some(format!("1{}", "0".repeat(DIGITS))),
        },
    ]
}

/// The addressed item, read back through the implemented reader.
async fn stored_item(router: &MultiServiceRouter, at: usize) -> Value {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key(at), "ConsistentRead": true}),
    )
    .await;
    read["Item"].clone()
}

#[tokio::test]
async fn a_computed_number_outside_the_domain_is_refused_and_writes_nothing() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    for (at, row) in rows().iter().enumerate() {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {
                "pk": {"S": format!("row-{at}")},
                "n": {"N": &row.stored},
            }}),
        )
        .await;
        let (status, body) = call(
            &router,
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": key(at),
                "UpdateExpression": row.expression,
                "ExpressionAttributeValues": {":v": {"N": &row.operand}},
            }),
        )
        .await;
        let item = stored_item(&router, at).await;
        match row.computed.as_deref() {
            Some(expected) => {
                assert_eq!(
                    status, 200,
                    "case {:?}: a result the domain holds is stored, got {status} with {body}",
                    row.case
                );
                assert_eq!(
                    item[row.target],
                    json!({"N": expected}),
                    "case {:?}: expected the computed number {expected:?}, got {}",
                    row.case,
                    item[row.target]
                );
            }
            None => {
                assert_eq!(
                    (status, error_code(&body).as_str(), message(&body).as_str()),
                    (400, "ValidationException", OVERFLOW),
                    "case {:?}: expected the numeric-overflow refusal, got {status} with \
                     {body}",
                    row.case
                );
                assert_eq!(
                    item["n"],
                    json!({"N": &row.stored}),
                    "case {:?}: a refused computation leaves the item where it was, got \
                     {item}",
                    row.case
                );
                assert_eq!(
                    item.get("tally"),
                    None,
                    "case {:?}: a refused computation writes no attribute, got {item}",
                    row.case
                );
            }
        }
    }
}

#[tokio::test]
async fn the_computed_number_refusal_is_stated_without_the_validation_envelope() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "bare"},
            "n": {"N": "9".repeat(DIGITS)},
        }}),
    )
    .await;

    // The clause names the rule the COMPUTED number broke and no member of the request:
    // nothing carried that number, so there is no member for the envelope to report a
    // constraint on.
    let (status, body) = call(
        &router,
        "UpdateItem",
        json!({
            "TableName": TABLE,
            "Key": {"pk": {"S": "bare"}},
            "UpdateExpression": "SET n = n + :v",
            "ExpressionAttributeValues": {":v": {"N": "2"}},
        }),
    )
    .await;
    let stated = message(&body);
    assert_eq!(
        (status, error_code(&body).as_str(), stated.as_str()),
        (400, "ValidationException", OVERFLOW),
        "case a computed number past the domain: expected the bare sentence, got {status} \
         with {body}"
    );
    assert!(
        !stated.starts_with(ENVELOPE),
        "case a computed number past the domain: expected no validation envelope, got \
         {stated:?}"
    );

    // The same arithmetic inside a TRANSACTION is read by the same evaluator, so the
    // action that stated it carries the same sentence as its cancellation reason.
    let (status, body) = call(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [{"Update": {
            "TableName": TABLE,
            "Key": {"pk": {"S": "bare"}},
            "UpdateExpression": "SET n = n + :v",
            "ExpressionAttributeValues": {":v": {"N": "2"}},
        }}]}),
    )
    .await;
    assert_eq!(
        (
            status,
            error_code(&body).as_str(),
            body["CancellationReasons"][0]["Code"].as_str(),
        ),
        (400, "TransactionCanceledException", Some("ValidationError")),
        "case a transactional computation past the domain: expected the action cancelled, \
         got {status} with {body}"
    );
    let reason = body["CancellationReasons"][0]["Message"]
        .as_str()
        .unwrap_or("<no reason>");
    assert!(
        reason.contains(OVERFLOW),
        "case a transactional computation past the domain: expected the overflow sentence, \
         got {reason:?}"
    );
}
