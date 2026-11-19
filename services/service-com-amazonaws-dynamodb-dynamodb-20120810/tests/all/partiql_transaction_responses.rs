// SPDX-License-Identifier: CC0-1.0

//! What `ExecuteTransaction` reports in `Responses`: the ITEMS its statements returned.
//!
//! `ItemResponse` carries a single optional `Item` and nothing else, so an entry for a
//! statement that returned none states nothing a caller can read. The rule the rows below
//! assert is that the list holds one entry per item a statement returned — and a WRITE
//! statement of a transaction returns none: the transaction is applied as a whole, so no
//! statement of it has an image of its own to report, and the clause that asks for one is
//! refused behind the slot that carried it rather than served. A READ returns the item its
//! key addresses, with an empty response where that key holds none, exactly as a
//! transactional read reports a missing item.
//!
//! The rows differ in how many statements the transaction carries, in which verb writes,
//! in which `RETURNING` form the refused statements ask for, and in which slot carries
//! the clause.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, message, ok};
use crate::table_items::create_active_table;

/// The table every statement below addresses.
const TABLE: &str = "ledger";

/// One `TransactStatements` entry.
fn statement(text: &str) -> Value {
    json!({"Statement": text})
}

/// The `Responses` entries a transaction reported, in the order it reported them.
fn responses(body: &Value) -> Vec<Value> {
    body["Responses"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("the transaction reports a Responses list, got {body}"))
}

/// Run one transaction that must succeed.
async fn transact(router: &MultiServiceRouter, case: &str, statements: Vec<Value>) -> Value {
    let (status, body) = call(
        router,
        "ExecuteTransaction",
        json!({"TransactStatements": statements}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// An emulator holding one table with three items in it.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("id", "HASH", "S")]).await;
    for (id, balance) in [("a", 10), ("b", 20), ("c", 30)] {
        ok(
            &router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"id": {"S": id}, "balance": {"N": balance.to_string()}},
            }),
        )
        .await;
    }
    router
}

#[tokio::test]
async fn a_transaction_reports_one_response_per_item_its_statements_returned() {
    let router = seeded().await;

    // Each row: the statements, and the whole `Responses` list the transaction must
    // report for them. Every write reports nothing, whichever verb it is and however many
    // statements the transaction carries.
    let rows: Vec<(&str, Vec<Value>, Vec<Value>)> = vec![
        (
            "two writes, an insert and an update",
            vec![
                statement("INSERT INTO ledger VALUE {'id': 'fresh', 'balance': 1}"),
                statement("UPDATE ledger SET balance = 11 WHERE id = 'a'"),
            ],
            vec![],
        ),
        (
            "two updates, over two items",
            vec![
                statement("UPDATE ledger SET balance = 13 WHERE id = 'a'"),
                statement("UPDATE ledger SET balance = 22 WHERE id = 'b'"),
            ],
            vec![],
        ),
        (
            "a delete of the item an earlier transaction inserted",
            vec![statement("DELETE FROM ledger WHERE id = 'fresh'")],
            vec![],
        ),
        (
            "a delete of a stored item",
            vec![statement("DELETE FROM ledger WHERE id = 'c'")],
            vec![],
        ),
    ];

    for (case, statements, expected) in rows {
        let body = transact(&router, case, statements).await;
        assert_eq!(
            responses(&body),
            expected,
            "case {case}: expected the responses {expected:?}, got {body}"
        );
    }

    // A READ reports the item its key addresses: one entry per statement, and an empty
    // response for the key that holds none.
    let read = transact(
        &router,
        "a read of a stored key and of an absent one",
        vec![
            statement("SELECT * FROM ledger WHERE id = 'a'"),
            statement("SELECT * FROM ledger WHERE id = 'absent'"),
            statement("SELECT balance FROM ledger WHERE id = 'b'"),
        ],
    )
    .await;
    assert_eq!(
        responses(&read),
        vec![
            json!({"Item": {"id": {"S": "a"}, "balance": {"N": "13"}}}),
            json!({}),
            json!({"Item": {"balance": {"N": "22"}}}),
        ],
        "case a read of a stored key and of an absent one: one response per statement, got \
         {read}"
    );

    // The writes above landed, so the entries each row reported are the items its
    // statements really returned.
    for (case, id, stored) in [
        ("the updated item", "a", json!({"id": {"S": "a"}, "balance": {"N": "13"}})),
        ("the second updated item", "b", json!({"id": {"S": "b"}, "balance": {"N": "22"}})),
        ("the deleted item", "c", Value::Null),
        ("the inserted item a later statement deleted", "fresh", Value::Null),
    ] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"id": {"S": id}}}),
        )
        .await;
        assert_eq!(
            read.get("Item").cloned().unwrap_or(Value::Null),
            stored,
            "case {case}: expected the store to hold {stored}, got {read}"
        );
    }
}

#[tokio::test]
async fn a_statement_asking_for_an_image_is_refused_behind_its_slot() {
    let router = seeded().await;

    // Each row: the case, the statements the transaction carries, and the zero-based
    // position of the one asking for an image. The rows span the verb that asks, the
    // `RETURNING` form it asks for, and where in the list it sits.
    let rows: Vec<(&str, Vec<Value>, usize)> = vec![
        (
            "an UPDATE asking for the image it leaves",
            vec![statement(
                "UPDATE ledger SET balance = 12 WHERE id = 'a' RETURNING ALL NEW *",
            )],
            0,
        ),
        (
            "an UPDATE asking for the image it replaced",
            vec![statement(
                "UPDATE ledger SET balance = 12 WHERE id = 'a' RETURNING ALL OLD *",
            )],
            0,
        ),
        (
            "an UPDATE asking for the attributes it modified",
            vec![statement(
                "UPDATE ledger SET balance = 12 WHERE id = 'a' RETURNING MODIFIED NEW *",
            )],
            0,
        ),
        (
            "a DELETE asking for the item it removed",
            vec![statement(
                "DELETE FROM ledger WHERE id = 'b' RETURNING ALL OLD *",
            )],
            0,
        ),
        (
            "the second statement of two asking for an image",
            vec![
                statement("UPDATE ledger SET balance = 14 WHERE id = 'a'"),
                statement("DELETE FROM ledger WHERE id = 'b' RETURNING ALL OLD *"),
            ],
            1,
        ),
        (
            "the third statement of three asking for an image",
            vec![
                statement("UPDATE ledger SET balance = 14 WHERE id = 'a'"),
                statement("DELETE FROM ledger WHERE id = 'b'"),
                statement(
                    "UPDATE ledger SET balance = 31 WHERE id = 'c' RETURNING ALL NEW *",
                ),
            ],
            2,
        ),
    ];

    for (case, statements, slot) in rows {
        let (status, body) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": statements}),
        )
        .await;
        let expected = format!(
            "Validation failed in TransactStatements[{slot}]: RETURNING clause is not supported \
             in ExecuteTransaction."
        );
        assert_eq!(
            (status, message(&body).as_str()),
            (400, expected.as_str()),
            "case {case}: expected the message {expected:?}, got {status} with {body}"
        );
    }

    // Every row was refused BEFORE anything was applied, so the three seeded items stand
    // exactly as they were — including the ones the well-formed statements beside the
    // refused clause addressed.
    for (id, balance) in [("a", "10"), ("b", "20"), ("c", "30")] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"id": {"S": id}}}),
        )
        .await;
        assert_eq!(
            read["Item"],
            json!({"id": {"S": id}, "balance": {"N": balance}}),
            "case a refused clause commits nothing: {id} must still hold {balance}, got {read}"
        );
    }
}
