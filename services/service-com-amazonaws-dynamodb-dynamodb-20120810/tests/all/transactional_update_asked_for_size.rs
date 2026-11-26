// SPDX-License-Identifier: CC0-1.0

//! End-to-end suite for WHO answers for the item-size quota when a `TransactWriteItems`
//! `Update` crosses it, driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<Operation>`).
//!
//! RULE: the item an `Update` ASKS FOR — the `Key` map it addresses plus what its own clauses
//! write — is a fact of the REQUEST, which no stored item can make smaller. A statement whose
//! own ask cannot fit in an item is therefore refused as the request it is: HTTP 400, the
//! validation class, the sentence the update path words that quota with and no
//! `CancellationReasons` at all, exactly as the single-item `UpdateItem` of the same statement
//! is refused. Only where the statement itself fits can the crossing be the transaction's own
//! doing — the bytes the STORED item already held are what carry it over — and that is "an
//! item size becomes too large (larger than 400 KB) [...] or a similar validation error occurs
//! because of changes made by the transaction", one of the circumstances
//! `TransactionCanceledException` documents, so the transaction is CANCELLED with that action
//! named in the reason list. Nothing of either answer is applied.
//!
//! The rows span what the rule generalises over: HOW the statement states its payload (one
//! value, two clauses that only cross together, a value the update COMPUTES from operands the
//! request carried, a value that fits on its own and only crosses beside the key it is written
//! next to), whether the addressed key HOLDS an item at all, and where the bytes that cross
//! the quota came from. The boundary is driven beside them: the largest statement the ceiling
//! admits commits through the transaction and reads back.
//!
//! Source of the quota: "Service, Account, and Table Quotas" in the Amazon DynamoDB Developer
//! Guide ("The maximum item size in DynamoDB is 400 KB"). Source of the cancel circumstance:
//! `TransactWriteItems`' own `TransactionCanceledException`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, error_type, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "TxnAskedForSize";

/// The documented item quota, 400 KB.
const MAX_ITEM_BYTES: usize = 400 * 1024;

/// AWS's fixed sentence for a change that would leave the item past the quota.
const UPDATE_TOO_BIG: &str = "Item size to update has exceeded the maximum allowed size";

/// The whole shape id the wire carries for the validation class, which is what a client
/// branching on `__type` matches.
const VALIDATION_SHAPE_ID: &str = "com.amazon.coral.validate#ValidationException";

/// The `CancellationReason$Code` an action that broke a rule of the service reports.
const VALIDATION_ERROR_CODE: &str = "ValidationError";

async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    router
}

fn key_of(key: &str) -> Value {
    json!({"pk": {"S": key}})
}

/// Store an item under `key` carrying `bytes` of filler.
async fn seed_item(router: &MultiServiceRouter, key: &str, bytes: usize) {
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": key}, "held": {"S": "h".repeat(bytes)}}}),
    )
    .await;
}

/// The `Update` request one statement makes against `key`.
fn update_of(key: &str, expression: &str, values: Value) -> Value {
    json!({"TableName": TABLE, "Key": key_of(key),
           "UpdateExpression": expression,
           "ExpressionAttributeValues": values})
}

/// The filler the item under `key` holds, or `None` where it holds no such attribute.
async fn held_bytes(router: &MultiServiceRouter, key: &str) -> Option<usize> {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key_of(key), "ConsistentRead": true}),
    )
    .await;
    read["Item"]["held"]["S"].as_str().map(str::len)
}

/// One statement, as the rows state it: the expression and the values it binds.
struct Statement {
    case: &'static str,
    /// The tag each row's key is built from, so every row addresses its own item.
    slot: &'static str,
    /// The bytes the addressed key is seeded with first (none when it holds no item).
    seeded: usize,
    expression: &'static str,
    values: fn() -> Value,
}

/// The statements whose own ask cannot fit in an item. They differ in HOW the payload is
/// stated and in whether the key they address already holds an item, which the answer does
/// not depend on.
fn asks_for_too_much() -> Vec<Statement> {
    vec![
        Statement {
            case: "one value past the quota, on a key that holds nothing",
            slot: "a1",
            seeded: 0,
            expression: "SET big = :v",
            values: || json!({":v": {"S": "x".repeat(MAX_ITEM_BYTES)}}),
        },
        Statement {
            case: "one value past the quota, on a key that holds an item",
            slot: "a2",
            seeded: 1024,
            expression: "SET big = :v",
            values: || json!({":v": {"S": "x".repeat(MAX_ITEM_BYTES)}}),
        },
        Statement {
            case: "two clauses that cross the quota only together",
            slot: "a3",
            seeded: 0,
            expression: "SET one = :a, two = :b",
            values: || {
                json!({":a": {"S": "x".repeat(210 * 1024)},
                       ":b": {"S": "y".repeat(210 * 1024)}})
            },
        },
        Statement {
            case: "a value the update computes from two operands it carried",
            slot: "a4",
            seeded: 0,
            expression: "SET joined = list_append(:a, :b)",
            values: || {
                json!({":a": {"L": [{"S": "x".repeat(210 * 1024)}]},
                       ":b": {"L": [{"S": "y".repeat(210 * 1024)}]}})
            },
        },
        Statement {
            // A value an item has room for on its own: what crosses the quota is the value
            // together with the key it is written beside, and the frames the clause costs.
            case: "a value that only crosses the quota beside its key",
            slot: "a5",
            seeded: 0,
            expression: "SET big = :v",
            values: || json!({":v": {"S": "x".repeat(MAX_ITEM_BYTES - 8)}}),
        },
    ]
}

/// The statements that FIT, and cross the quota only over the bytes the stored item already
/// held. They differ in where those bytes meet the statement: a value added beside them, and
/// a value the update computes from one of them.
fn crosses_over_the_stored_item() -> Vec<Statement> {
    vec![
        Statement {
            case: "a value any item has room for, over bytes the item already held",
            slot: "s1",
            seeded: 300 * 1024,
            expression: "SET big = :v",
            values: || json!({":v": {"S": "x".repeat(150 * 1024)}}),
        },
        Statement {
            case: "a computed value that fits, over bytes the item already held",
            slot: "s2",
            seeded: 210 * 1024,
            expression: "SET joined = list_append(:a, :a)",
            values: || json!({":a": {"L": [{"S": "x".repeat(100 * 1024)}]}}),
        },
    ]
}

/// Assert the CALL was refused as the request it is: the status, the whole validation shape
/// id, the sentence, and no reason list — a refused call grades no action.
fn assert_refused(case: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, error_type(body).as_str(), message(body).as_str()),
        (400, VALIDATION_SHAPE_ID, UPDATE_TOO_BIG),
        "case {case:?}: expected 400 {VALIDATION_SHAPE_ID} {UPDATE_TOO_BIG:?}, got {status} \
         with {body}"
    );
    assert!(
        body.get("CancellationReasons").is_none(),
        "case {case:?}: a refused call grades no action, so it reports no reason list, got \
         {body}"
    );
}

#[tokio::test]
async fn a_statement_that_cannot_fit_in_an_item_refuses_the_call() {
    let router = seeded().await;

    for statement in asks_for_too_much() {
        let key = format!("{}-txn", statement.slot);
        if statement.seeded > 0 {
            seed_item(&router, &key, statement.seeded).await;
        }
        let request = update_of(&key, statement.expression, (statement.values)());
        let (status, body) = call(
            &router,
            "TransactWriteItems",
            json!({"TransactItems": [{"Update": request.clone()}]}),
        )
        .await;
        assert_refused(statement.case, status, &body);

        // The single-item surface of the SAME statement answers the same: the ceiling is the
        // item's and the refusal is the request's, so the two paths stop at one byte with one
        // class and one sentence.
        let (status, body) = call(&router, "UpdateItem", request).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", UPDATE_TOO_BIG),
            "case {:?}: expected UpdateItem to refuse the same statement, got {status} with \
             {body}",
            statement.case
        );

        // Nothing of either answer landed.
        let held = held_bytes(&router, &key).await;
        let expected = (statement.seeded > 0).then_some(statement.seeded);
        assert_eq!(
            held, expected,
            "case {:?}: a refused statement leaves the stored item where it was, expected \
             {expected:?} byte(s) of filler, got {held:?}",
            statement.case
        );
    }
}

#[tokio::test]
async fn a_statement_that_fits_but_grows_the_stored_item_cancels_the_transaction() {
    let router = seeded().await;

    for statement in crosses_over_the_stored_item() {
        let key = format!("{}-txn", statement.slot);
        seed_item(&router, &key, statement.seeded).await;
        let (status, body) = call(
            &router,
            "TransactWriteItems",
            json!({"TransactItems": [
                {"Update": update_of(&key, statement.expression, (statement.values)())}
            ]}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (
                400,
                "TransactionCanceledException",
                "Transaction cancelled, please refer cancellation reasons for specific reasons \
                 [ValidationError]"
            ),
            "case {:?}: expected the transaction cancelled over that action, got {status} with \
             {body}",
            statement.case
        );
        assert_eq!(
            (
                body["CancellationReasons"][0]["Code"].as_str(),
                body["CancellationReasons"][0]["Message"].as_str(),
            ),
            (Some(VALIDATION_ERROR_CODE), Some(UPDATE_TOO_BIG)),
            "case {:?}: expected that action's reason to word the quota, got {body}",
            statement.case
        );

        let held = held_bytes(&router, &key).await;
        assert_eq!(
            held,
            Some(statement.seeded),
            "case {:?}: a cancelled transaction leaves the stored item where it was, expected \
             {} byte(s) of filler, got {held:?}",
            statement.case,
            statement.seeded
        );
    }
}

#[tokio::test]
async fn a_statement_inside_the_quota_commits_through_the_transaction() {
    // The boundary on the served side, so the refusals above are the quota and not a blanket
    // rejection: a statement comfortably inside it, and one whose payload is the largest a
    // single `SET` of this shape is admitted with, both commit and read back.
    let router = seeded().await;
    for (case, key, bytes) in [
        ("a small statement", "fits-small", 2048_usize),
        (
            "the largest payload this statement shape is admitted with",
            "fits-largest",
            MAX_ITEM_BYTES - 64,
        ),
    ] {
        ok(
            &router,
            "TransactWriteItems",
            json!({"TransactItems": [
                {"Update": update_of(key, "SET held = :v",
                                     json!({":v": {"S": "h".repeat(bytes)}}))}
            ]}),
        )
        .await;
        let held = held_bytes(&router, key).await;
        assert_eq!(
            held,
            Some(bytes),
            "case {case:?}: expected the committed statement to read back whole, got {held:?} \
             byte(s) of filler"
        );
    }
}
