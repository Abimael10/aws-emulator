// SPDX-License-Identifier: CC0-1.0

//! The two readers that report an `ItemResponse` AGREE: a transactional get and a PartiQL
//! read transaction answer the same element the same way, driven end to end over the wire.
//!
//! RULE under test: what an `ItemResponse` states is a fact of that SHAPE — it carries one
//! optional `Item` and nothing else, so "if a requested item could not be retrieved, the
//! corresponding ItemResponse object is Null, or if the requested item has no projected
//! attributes, the corresponding ItemResponse object is an empty Map"
//! (`TransactGetItems$Responses`) — and NOT a fact of either operation that reports one. So
//! for one stored item and one projection, `TransactGetItems` and `ExecuteTransaction` report
//! the same `Responses` entry: the same pairs where there are pairs, and the same empty
//! object where there are none.
//!
//! Each half is pinned on its own beside this file — `transactional_item_response_presence`
//! over every reason a GET comes to no pair and every position such a get can take, and
//! `item_response_projected_pairs` over the PartiQL statements — which is what leaves this
//! one assertion to make: that the two cannot drift apart. A rule stated once in the shape
//! but implemented twice is exactly where one surface keeps answering the old way.
//!
//! The rows differ along what the rule generalises over: why the element comes to no pair (an
//! attribute the item does not hold, a key that holds no item), and the case where it does
//! come to one, so the agreement is read for both answers.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::table_items::create_active_table;

/// The table both readers address: one string partition key, named in lower case so a PartiQL
/// statement can address it unquoted.
const TABLE: &str = "agreeing_pairs";

/// The emulator holding that table with one item in it.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": "seed"}, "note": {"S": "hello"}},
        }),
    )
    .await;
    router
}

/// The `Responses` list a transactional GET of `key` under `projection` reports.
async fn transactional_get(
    router: &MultiServiceRouter,
    key: &str,
    projection: &str,
) -> Value {
    let answered = ok(
        router,
        "TransactGetItems",
        json!({
            "TransactItems": [{
                "Get": {
                    "TableName": TABLE,
                    "Key": {"pk": {"S": key}},
                    "ProjectionExpression": projection,
                },
            }],
        }),
    )
    .await;
    answered["Responses"].clone()
}

/// The `Responses` list the equivalent PartiQL read transaction reports.
async fn partiql_read(router: &MultiServiceRouter, key: &str, projection: &str) -> Value {
    let answered = ok(
        router,
        "ExecuteTransaction",
        json!({
            "TransactStatements": [{
                "Statement": format!("SELECT {projection} FROM {TABLE} WHERE pk = '{key}'"),
            }],
        }),
    )
    .await;
    answered["Responses"].clone()
}

#[tokio::test]
async fn both_readers_report_the_same_item_response_for_the_same_element() {
    let router = seeded().await;

    for (case, key, projection, expected) in [
        (
            "a projection naming an attribute the item does not hold",
            "seed",
            "absent",
            json!([{}]),
        ),
        (
            "a key the table holds no item for",
            "absent",
            "note",
            json!([{}]),
        ),
        (
            "a projection naming an attribute the item does hold",
            "seed",
            "note",
            json!([{"Item": {"note": {"S": "hello"}}}]),
        ),
    ] {
        let transactional = transactional_get(&router, key, projection).await;
        let partiql = partiql_read(&router, key, projection).await;
        assert_eq!(
            transactional, expected,
            "case {case:?}: TransactGetItems must report {expected:?}, got {transactional}"
        );
        assert_eq!(
            partiql, transactional,
            "case {case:?}: the PartiQL read transaction must report what the transactional \
             get reports ({transactional}), got {partiql}"
        );
    }

    // Neither read narrowed what the store holds.
    let whole = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "seed"}}}),
    )
    .await;
    assert_eq!(
        whole["Item"],
        json!({"pk": {"S": "seed"}, "note": {"S": "hello"}}),
        "the projected reads must leave the stored item whole, got {whole}"
    );
}
