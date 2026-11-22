// SPDX-License-Identifier: CC0-1.0

//! That the TWO readers which answer in the `ItemResponse` envelope answer one fact the SAME
//! way, driven end to end over the wire.
//!
//! RULE under test: whether an `ItemResponse` carries an `Item` is a fact of that STRUCTURE —
//! "if a requested item could not be retrieved, the corresponding ItemResponse object is
//! Null, or if the requested item has no projected attributes, the corresponding ItemResponse
//! object is an empty Map" (`TransactGetItems$Responses`;
//! `smithy_query(query="shape", shape="ItemResponse")` is one OPTIONAL `Item` member and
//! nothing else) — so it cannot depend on WHICH reader built it. `TransactGetItems` narrowing
//! a get with a `ProjectionExpression` and `ExecuteTransaction` narrowing a `SELECT` with the
//! same attribute list therefore report the same object for the same item: the empty one
//! where the read came to no projected pair, and the pairs it did come to otherwise.
//!
//! Each reader's own axis is driven by a suite of its own — every reason a get comes to no
//! pair and the position of such a get among its neighbours by
//! `transactional_item_response_presence`, the PartiQL rows by `item_response_projected_pairs`
//! — and what a reader whose answer IS the projected map does instead (`GetItem`,
//! `BatchGetItem` and the list elements of `Query` and `Scan`, which carry the empty map) by
//! `projected_item_member_absence`. What is pinned HERE is only that the two envelope readers
//! agree, over rows that differ in how many of the named attributes the item holds (none, one
//! of two, all) and in whether the key addressed holds an item at all.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::table_items::create_active_table;

/// The table both readers below address: one string partition key, named in lower case so a
/// PartiQL statement can address it unquoted.
const TABLE: &str = "envelopereaders";

/// The item the projections are applied to.
fn stored_item() -> Value {
    json!({"pk": {"S": "seed"}, "note": {"S": "hello"}, "tally": {"N": "3"}})
}

/// The emulator holding that table and that item.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": stored_item()}),
    )
    .await;
    router
}

/// One row: the case, the attributes both readers are asked for, the partition they address,
/// and the whole `ItemResponse` object each must report.
struct Row {
    case: &'static str,
    attributes: &'static str,
    partition: &'static str,
    response: Value,
}

/// The rows, over how many of the named attributes the addressed item holds.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "none of the named attributes",
            attributes: "absent",
            partition: "seed",
            response: json!({}),
        },
        Row {
            case: "none of two named attributes",
            attributes: "absent, vanished",
            partition: "seed",
            response: json!({}),
        },
        Row {
            case: "an attribute of a key that holds no item",
            attributes: "note",
            partition: "nothing",
            response: json!({}),
        },
        Row {
            case: "one of two named attributes",
            attributes: "note, absent",
            partition: "seed",
            response: json!({"Item": {"note": {"S": "hello"}}}),
        },
        Row {
            case: "both named attributes",
            attributes: "note, tally",
            partition: "seed",
            response: json!({"Item": {"note": {"S": "hello"}, "tally": {"N": "3"}}}),
        },
    ]
}

#[tokio::test]
async fn both_readers_of_the_item_response_envelope_report_the_same_object() {
    let router = seeded().await;

    for row in rows() {
        let got = ok(
            &router,
            "TransactGetItems",
            json!({"TransactItems": [{"Get": {
                "TableName": TABLE,
                "Key": {"pk": {"S": row.partition}},
                "ProjectionExpression": row.attributes,
            }}]}),
        )
        .await;
        let selected = ok(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": [{"Statement": format!(
                "SELECT {} FROM {TABLE} WHERE pk = '{}'",
                row.attributes, row.partition
            )}]}),
        )
        .await;
        for (reader, answered) in [("TransactGetItems", &got), ("ExecuteTransaction", &selected)] {
            assert_eq!(
                answered["Responses"],
                json!([row.response.clone()]),
                "case {:?} ({reader}): expected the responses [{}], got {answered}",
                row.case,
                row.response
            );
        }
        assert_eq!(
            got["Responses"], selected["Responses"],
            "case {:?}: the two envelope readers must report the same object, got {got} and \
             {selected}",
            row.case
        );
    }

    // Neither reader narrows what the store holds, so the item the empty objects above were
    // read from is still whole.
    let whole = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "seed"}}}),
    )
    .await;
    assert_eq!(
        whole["Item"],
        stored_item(),
        "the projected reads must leave the stored item whole, got {whole}"
    );
}
