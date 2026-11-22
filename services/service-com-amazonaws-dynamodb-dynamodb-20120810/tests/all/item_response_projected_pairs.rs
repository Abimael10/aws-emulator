// SPDX-License-Identifier: CC0-1.0

//! What the PARTIQL read transaction answers for a slot it came to no projected attribute
//! for, driven end to end over the wire.
//!
//! RULE under test: an `ItemResponse` carries "a Map of the name-value pairs that are the
//! projected attributes of the requested item" and states having none of them as the
//! structure with nothing in it — "if a requested item could not be retrieved, the
//! corresponding ItemResponse object is Null, or if the requested item has no projected
//! attributes, the corresponding ItemResponse object is an empty Map"
//! (`TransactGetItems$Responses`). That is a fact of the `ItemResponse` SHAPE
//! (`smithy_query(query="shape", shape="ItemResponse")` — one OPTIONAL `Item` member and
//! nothing else), so it holds for every read that reports those structures: `ExecuteTransaction`
//! reports its `SELECT`s in the same list `TransactGetItems` reports its `Get`s in, and
//! answers a statement with no projected pair the same way.
//!
//! The transactional GET surface of the same rule — every reason a get comes to no pair, and
//! the position of such a get among its neighbours — is `transactional_item_response_presence`;
//! this suite is the PartiQL half, whose rows differ along the reason a statement projects
//! nothing (an attribute the item does not hold, several at once, a key that holds no item),
//! its position beside a statement that does project one, and how many statements the
//! transaction carries.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::table_items::create_active_table;

/// The table every statement below reads: one string partition key, named in lower case so a
/// statement can address it unquoted.
const TABLE: &str = "pairs";

/// The emulator holding that table with two items in it: the one every projection that
/// matches nothing is applied to, and a neighbour a statement beside it can project a pair
/// from — "Transaction requests cannot include multiple operations on one item".
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    for item in [
        json!({"pk": {"S": "seed"}, "note": {"S": "hello"}, "tally": {"N": "3"}}),
        json!({"pk": {"S": "other"}, "note": {"S": "hello"}}),
    ] {
        ok(&router, "PutItem", json!({"TableName": TABLE, "Item": item})).await;
    }
    router
}

/// The `ItemResponse` a statement that selects the neighbour's stored attribute reports.
fn projected_pair() -> Value {
    json!({"Item": {"note": {"S": "hello"}}})
}

#[tokio::test]
async fn a_statement_with_no_projected_pair_answers_the_empty_item_response() {
    let router = seeded().await;

    for (case, statements, expected) in [
        (
            "an attribute the item does not hold",
            vec![format!("SELECT absent FROM {TABLE} WHERE pk = 'seed'")],
            json!([{}]),
        ),
        (
            "two such attributes at once",
            vec![format!(
                "SELECT absent, vanished FROM {TABLE} WHERE pk = 'seed'"
            )],
            json!([{}]),
        ),
        (
            "a key that holds no item",
            vec![format!("SELECT note FROM {TABLE} WHERE pk = 'absent'")],
            json!([{}]),
        ),
        (
            "such a statement before one that selects a stored attribute",
            vec![
                format!("SELECT absent FROM {TABLE} WHERE pk = 'seed'"),
                format!("SELECT note FROM {TABLE} WHERE pk = 'other'"),
            ],
            json!([{}, {"Item": {"note": {"S": "hello"}}}]),
        ),
        (
            "such a statement after one that selects a stored attribute",
            vec![
                format!("SELECT note FROM {TABLE} WHERE pk = 'other'"),
                format!("SELECT absent FROM {TABLE} WHERE pk = 'seed'"),
            ],
            json!([{"Item": {"note": {"S": "hello"}}}, {}]),
        ),
        (
            "two statements with no pair beside one with a pair",
            vec![
                format!("SELECT absent FROM {TABLE} WHERE pk = 'seed'"),
                format!("SELECT note FROM {TABLE} WHERE pk = 'absent'"),
                format!("SELECT note FROM {TABLE} WHERE pk = 'other'"),
            ],
            json!([{}, {}, {"Item": {"note": {"S": "hello"}}}]),
        ),
        (
            "a statement that selects a stored attribute",
            vec![format!("SELECT note FROM {TABLE} WHERE pk = 'other'")],
            Value::Array(vec![projected_pair()]),
        ),
        (
            "a statement that selects the whole item",
            vec![format!("SELECT * FROM {TABLE} WHERE pk = 'other'")],
            Value::Array(vec![json!({"Item": {"pk": {"S": "other"},
                                             "note": {"S": "hello"}}})]),
        ),
    ] {
        let entries: Vec<Value> = statements
            .iter()
            .map(|text| json!({"Statement": text}))
            .collect();
        let answered = ok(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": entries}),
        )
        .await;
        assert_eq!(
            answered["Responses"], expected,
            "case {case:?}: expected the responses {expected}, got {answered}"
        );
    }
}

#[tokio::test]
async fn the_read_transactions_leave_the_stored_items_whole() {
    let router = seeded().await;

    ok(
        &router,
        "ExecuteTransaction",
        json!({"TransactStatements": [
            {"Statement": format!("SELECT absent FROM {TABLE} WHERE pk = 'seed'")},
            {"Statement": format!("SELECT note FROM {TABLE} WHERE pk = 'other'")},
        ]}),
    )
    .await;
    // A read reports what a projection came to; it never narrows what the store holds, so the
    // item the empty `ItemResponse` above was read from is still whole.
    let whole = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "seed"}}}),
    )
    .await;
    assert_eq!(
        whole["Item"],
        json!({"pk": {"S": "seed"}, "note": {"S": "hello"}, "tally": {"N": "3"}}),
        "the projected reads must leave the stored item whole, got {whole}"
    );
}
