// SPDX-License-Identifier: CC0-1.0

//! WHICH ANSWER an update that would leave an item past the documented item-size ceiling
//! earns, across the surfaces an update arrives through.
//!
//! The rule under test: the ceiling is the ITEM's and does not move with the surface the
//! change arrived through, so an update whose resulting item would cross it is refused as the
//! request it is — HTTP 400, the validation class, and the sentence the update path words that
//! quota with — on the TRANSACTIONAL surfaces too, which are not cancelled and report no
//! `CancellationReasons`; and nothing of the refused call is applied.
//!
//! The rows span what the rule generalises over: the surface a change is stated through
//! (`UpdateItem` and a transaction's `Update` for an `UpdateExpression`, `ExecuteStatement`
//! and `ExecuteTransaction` for a PartiQL `UPDATE`), HOW the ceiling is crossed (one value
//! past it on its own, a value inside it that only crosses together with what the item
//! already holds, two clauses that cross only together, a value the update COMPUTES), and
//! WHERE the oversized action sits in a transaction that carries more than one.
//!
//! The BOUNDARIES are driven beside them: a change inside the ceiling COMMITS through a
//! transaction and reads back, an action whose own condition is not met still CANCELS the
//! transaction with its reason list (so the class split between a request's defect and an
//! action's failure is kept), and an item a `Put` CARRIES past the ceiling earns the sentence
//! for an item that was carried.
//!
//! Source of the ceiling: "Service, Account, and Table Quotas" in the Amazon DynamoDB
//! Developer Guide ("The maximum item size in DynamoDB is 400 KB").

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, error_type, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "TxnUpdateCeiling";

/// The documented item ceiling, 400 KB.
const MAX_ITEM_BYTES: usize = 400 * 1024;

/// AWS's fixed sentence for a change that would leave the item past the ceiling.
const UPDATE_TOO_BIG: &str = "Item size to update has exceeded the maximum allowed size";

/// AWS's fixed sentence for a request that CARRIED an item past the ceiling.
const ITEM_TOO_BIG: &str = "Item size has exceeded the maximum allowed size";

/// The whole shape id the wire carries for the validation class, which is what a client
/// branching on `__type` matches.
const VALIDATION_SHAPE_ID: &str = "com.amazon.coral.validate#ValidationException";

/// An emulator holding the table every row writes into.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    router
}

/// The key map addressing `key`.
fn key_of(key: &str) -> Value {
    json!({"pk": {"S": key}})
}

/// Store an item under `key` carrying `bytes` of filler, which is the state a row's change is
/// then read against.
async fn seed_item(router: &MultiServiceRouter, key: &str, bytes: usize) {
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": key}, "held": {"S": "h".repeat(bytes)}}}),
    )
    .await;
}

/// What a row's change is refused with, asserted as a client reads it: the status, the whole
/// validation shape id, the sentence, and the absence of a reason list — a refused call grades
/// no action, so a transaction that is refused carries no `CancellationReasons`.
fn assert_refused(case: &str, status: u16, body: &Value, sentence: &str) {
    assert_eq!(
        (status, error_type(body).as_str(), message(body).as_str()),
        (400, VALIDATION_SHAPE_ID, sentence),
        "case {case:?}: expected 400 {VALIDATION_SHAPE_ID} {sentence:?}, got {status} with {body}"
    );
    assert!(
        body.get("CancellationReasons").is_none(),
        "case {case:?}: a refused call grades no action, so it reports no reason list, got {body}"
    );
}

/// The length of the filler the item under `key` holds, as a `ConsistentRead` reports it, and
/// `None` where the item holds no such attribute at all.
async fn held_bytes(router: &MultiServiceRouter, key: &str) -> Option<usize> {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key_of(key), "ConsistentRead": true}),
    )
    .await;
    read["Item"]["held"]["S"].as_str().map(str::len)
}

/// One way of crossing the ceiling, stated as an `UpdateExpression`: the case, the bytes the
/// item is seeded with first, and the `(expression, substitutions)` the change states.
struct Crossing {
    case: &'static str,
    /// A short tag the row's key values are built from, so each row addresses its own item.
    slot: &'static str,
    seeded: usize,
    expression: &'static str,
    values: fn() -> Value,
}

/// The ways an `UpdateExpression` can leave the item past the ceiling. They differ in WHAT
/// crosses it: one value on its own, a value inside the ceiling over bytes the item already
/// holds, two clauses that only cross together, and a value the update computes from two
/// operands neither of which crosses it.
fn crossings() -> Vec<Crossing> {
    vec![
        Crossing {
            case: "one value past the ceiling on its own",
            slot: "c1",
            seeded: 0,
            expression: "SET big = :v",
            values: || json!({":v": {"S": "x".repeat(MAX_ITEM_BYTES)}}),
        },
        Crossing {
            case: "a value inside the ceiling over what the item already holds",
            slot: "c2",
            seeded: 300 * 1024,
            expression: "SET big = :v",
            values: || json!({":v": {"S": "x".repeat(150 * 1024)}}),
        },
        Crossing {
            case: "two clauses that cross the ceiling only together",
            slot: "c3",
            seeded: 0,
            expression: "SET one = :a, two = :b",
            values: || {
                json!({":a": {"S": "x".repeat(210 * 1024)},
                       ":b": {"S": "y".repeat(210 * 1024)}})
            },
        },
        Crossing {
            case: "a value the update computes past the ceiling",
            slot: "c4",
            seeded: 0,
            expression: "SET joined = list_append(:a, :b)",
            values: || {
                json!({":a": {"L": [{"S": "x".repeat(210 * 1024)}]},
                       ":b": {"L": [{"S": "y".repeat(210 * 1024)}]}})
            },
        },
    ]
}

#[tokio::test]
async fn an_update_expression_past_the_ceiling_is_refused_whichever_surface_states_it() {
    let router = seeded().await;

    // Each row is one crossing on one surface: the single-item update and the transaction's
    // `Update` are held to the SAME ceiling and answer in the same class and sentence, which
    // is the rule — the transaction is not cancelled for it.
    for crossing in crossings() {
        for (surface, transactional) in [("UpdateItem", false), ("TransactWriteItems", true)] {
            let case = format!("{} through {surface}", crossing.case);
            let key = format!("{}-{surface}", crossing.slot);
            if crossing.seeded > 0 {
                seed_item(&router, &key, crossing.seeded).await;
            }
            let update = json!({"TableName": TABLE, "Key": key_of(&key),
                                "UpdateExpression": crossing.expression,
                                "ExpressionAttributeValues": (crossing.values)()});
            let request = if transactional {
                json!({"TransactItems": [{"Update": update}]})
            } else {
                update
            };
            let (status, body) = call(&router, surface, request).await;
            assert_refused(&case, status, &body, UPDATE_TOO_BIG);

            // Nothing of the refused change landed: the item is exactly what it was seeded
            // with, and a key that held no item holds none.
            let held = held_bytes(&router, &key).await;
            let expected = (crossing.seeded > 0).then_some(crossing.seeded);
            assert_eq!(
                held, expected,
                "case {case:?}: a refused change leaves the stored item where it was, expected \
                 {expected:?} byte(s) of filler, got {held:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_partiql_update_past_the_ceiling_is_refused_whichever_surface_states_it() {
    let router = seeded().await;

    // The same two sides of the ceiling as above — a parameter past it on its own and one
    // inside it over bytes the item already holds — stated as a PartiQL `UPDATE`, run on its
    // own and inside a transaction. Every row addresses an item that is already there: a
    // PartiQL `UPDATE` states its target in a `WHERE` clause, so a key that holds no item is
    // a predicate nothing satisfies rather than a change to size.
    for (slot, case, seeded, bytes) in [
        (
            "p1",
            "one parameter past the ceiling on its own",
            16,
            MAX_ITEM_BYTES,
        ),
        (
            "p2",
            "a parameter inside the ceiling over what the item already holds",
            300 * 1024,
            150 * 1024,
        ),
    ] {
        for (surface, transactional) in [("ExecuteStatement", false), ("ExecuteTransaction", true)]
        {
            let case = format!("{case} through {surface}");
            let key = format!("{slot}-{surface}");
            seed_item(&router, &key, seeded).await;
            let statement = json!({
                "Statement": format!("UPDATE \"{TABLE}\" SET big = ? WHERE pk = '{key}'"),
                "Parameters": [{"S": "x".repeat(bytes)}],
            });
            let request = if transactional {
                json!({"TransactStatements": [statement]})
            } else {
                statement
            };
            let (status, body) = call(&router, surface, request).await;
            assert_refused(&case, status, &body, UPDATE_TOO_BIG);

            let held = held_bytes(&router, &key).await;
            assert_eq!(
                held,
                Some(seeded),
                "case {case:?}: a refused statement leaves the stored item where it was, \
                 expected {seeded} byte(s) of filler, got {held:?}"
            );
        }
    }
}

#[tokio::test]
async fn where_the_oversized_action_sits_does_not_change_the_answer() {
    let router = seeded().await;

    let oversize = json!({":v": {"S": "x".repeat(MAX_ITEM_BYTES)}});
    let update = |key: &str| {
        json!({"Update": {"TableName": TABLE, "Key": key_of(key),
               "UpdateExpression": "SET big = :v",
               "ExpressionAttributeValues": oversize.clone()}})
    };
    let put = |key: &str| json!({"Put": {"TableName": TABLE, "Item": key_of(key)}});
    let failing_check = || {
        json!({"ConditionCheck": {"TableName": TABLE, "Key": key_of("never-written"),
               "ConditionExpression": "attribute_exists(pk)"}})
    };

    // The rows differ in what stands BESIDE the oversized action: nothing, an action that
    // would succeed on either side of it, and an action whose condition does not hold —
    // which on its own would cancel the transaction. The refusal is the request's, so none of
    // them changes it.
    for (case, actions, written) in [
        ("the oversized action alone", json!([update("alone")]), vec![]),
        (
            "the oversized action before one that would succeed",
            json!([update("before"), put("after-the-ceiling")]),
            vec!["after-the-ceiling"],
        ),
        (
            "the oversized action after one that would succeed",
            json!([put("before-the-ceiling"), update("after")]),
            vec!["before-the-ceiling"],
        ),
        (
            "the oversized action beside a condition that does not hold",
            json!([failing_check(), update("beside")]),
            vec![],
        ),
    ] {
        let (status, body) = call(
            &router,
            "TransactWriteItems",
            json!({"TransactItems": actions}),
        )
        .await;
        assert_refused(case, status, &body, UPDATE_TOO_BIG);

        // Nothing a refused transaction carried was applied, the sibling `Put` included.
        for key in written {
            let read = ok(
                &router,
                "GetItem",
                json!({"TableName": TABLE, "Key": key_of(key), "ConsistentRead": true}),
            )
            .await;
            assert!(
                read.get("Item").is_none(),
                "case {case:?}: the refused call wrote nothing, got {read}"
            );
        }
    }
}

#[tokio::test]
async fn the_classes_either_side_of_the_ceiling_refusal_are_kept() {
    let router = seeded().await;
    seed_item(&router, "committed", 1024).await;

    // A change INSIDE the ceiling commits through the transaction and reads back, so the
    // refusals above are the ceiling and not a blanket rejection of a transactional update.
    ok(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [{"Update": {"TableName": TABLE, "Key": key_of("committed"),
               "UpdateExpression": "SET held = :v",
               "ExpressionAttributeValues": {":v": {"S": "h".repeat(2048)}}}}]}),
    )
    .await;
    let held = held_bytes(&router, "committed").await;
    assert_eq!(
        held,
        Some(2048),
        "case \"a change inside the ceiling\": expected the applied change to read back, got \
         {held:?} byte(s) of filler"
    );

    // An action whose own CONDITION is not met is still that action's failure, so the
    // transaction is CANCELLED and the reason list names it: the class split between a
    // request that cannot be served and an action that failed is what the ceiling refusal
    // sits on one side of.
    let (status, body) = call(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [{"Update": {"TableName": TABLE, "Key": key_of("committed"),
               "UpdateExpression": "SET held = :v",
               "ConditionExpression": "attribute_not_exists(pk)",
               "ExpressionAttributeValues": {":v": {"S": "h"}}}}]}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "TransactionCanceledException"),
        "case \"an action whose condition is not met\": expected the transaction cancelled, got \
         {status} with {body}"
    );
    assert_eq!(
        (
            body["CancellationReasons"][0]["Code"].as_str(),
            body["CancellationReasons"][0]["Message"].as_str(),
        ),
        (
            Some("ConditionalCheckFailed"),
            Some("The conditional request failed")
        ),
        "case \"an action whose condition is not met\": expected that action's reason, got {body}"
    );
    let held = held_bytes(&router, "committed").await;
    assert_eq!(
        held,
        Some(2048),
        "case \"an action whose condition is not met\": a cancelled transaction wrote nothing, \
         got {held:?} byte(s) of filler"
    );

    // An item a `Put` CARRIES past the ceiling is too large as submitted, so it earns the
    // carried-item sentence in the same class.
    let (status, body) = call(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [{"Put": {"TableName": TABLE, "Item": {
            "pk": {"S": "carried"},
            "big": {"S": "x".repeat(MAX_ITEM_BYTES + 1)},
        }}}]}),
    )
    .await;
    assert_refused(
        "an item the request carried past the ceiling",
        status,
        &body,
        ITEM_TOO_BIG,
    );
}
