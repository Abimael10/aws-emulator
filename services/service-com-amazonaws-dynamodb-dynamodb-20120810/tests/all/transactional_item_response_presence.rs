// SPDX-License-Identifier: CC0-1.0

//! What ONE element of a transactional read reports, driven end to end.
//!
//! RULE under test: an `ItemResponse` carries the PROJECTED ATTRIBUTES of the item its `Get`
//! addressed, so it carries an `Item` only where that get has projected attributes to report
//! — "if a requested item could not be retrieved, the corresponding ItemResponse object is
//! Null, or if the requested item has no projected attributes, the corresponding ItemResponse
//! object is an empty Map" (`TransactGetItems$Responses`). A key that holds no item and a
//! projection that named no location of the item it found therefore answer alike: the empty
//! `ItemResponse`, with no `Item` member in it.
//!
//! The rows differ along what the rule generalises over:
//!
//! * WHY the get projected nothing: an attribute the item does not hold, that attribute
//!   behind an alias, a path into a map that holds nothing there, a path past the end of a
//!   stored list, a path through a scalar, several such paths at once, and a key holding no
//!   item at all;
//! * WHAT the item holds, so a projection can miss inside each container shape;
//! * the POSITION of such a get in the transaction — alone, before a get that does project
//!   something, and after one — because each `ItemResponse` "corresponds to the
//!   TransactGetItem object in the same position" and is decided on its own;
//!
//! each beside a projection that DOES match, so the rows show both answers for one reader
//! and one item.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::table_items::create_active_table;

/// The table every element below reads: one string partition key.
const TABLE: &str = "TransactItemResponsePresence";

/// The stored item every projection is applied to: attributes of several shapes, so a path
/// can miss inside a map, past the end of a list and through a scalar.
fn stored_item() -> Value {
    json!({
        "pk": {"S": "seed"},
        "note": {"S": "hello"},
        "tally": {"N": "3"},
        "doc": {"M": {"leaf": {"S": "deep"}}},
        "parts": {"L": [{"S": "zero"}, {"S": "one"}]},
    })
}

/// A second item, under its own key: "Transaction requests cannot include multiple
/// operations on one item", so a row read BESIDE a get that projects something needs that
/// get to address another item.
fn neighbour_item() -> Value {
    json!({"pk": {"S": "other"}, "note": {"S": "hello"}})
}

/// The emulator holding that one table and those two items.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    for item in [stored_item(), neighbour_item()] {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": item}),
        )
        .await;
    }
    router
}

/// One `Get` element: the partition value it addresses, the projection it states, and its
/// aliases.
fn get(partition: &str, expression: &str, names: Value) -> Value {
    let mut element = json!({
        "TableName": TABLE,
        "Key": {"pk": {"S": partition}},
        "ProjectionExpression": expression,
    });
    if !names.is_null() {
        element["ExpressionAttributeNames"] = names;
    }
    json!({"Get": element})
}

/// One row: the case, the element, and the `Item` the element must report — `None` where the
/// `ItemResponse` must be the empty object.
struct Row {
    case: &'static str,
    element: Value,
    reported: Option<Value>,
}

/// The rows, over the projections that report nothing and the ones that do.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an attribute the item does not hold",
            element: get("seed", "absent", Value::Null),
            reported: None,
        },
        Row {
            case: "that attribute behind an alias",
            element: get("seed", "#a", json!({"#a": "absent"})),
            reported: None,
        },
        Row {
            case: "a path into a map that holds nothing there",
            element: get("seed", "doc.gap", Value::Null),
            reported: None,
        },
        Row {
            case: "a path past the end of a stored list",
            element: get("seed", "parts[9]", Value::Null),
            reported: None,
        },
        Row {
            case: "a path through a scalar, which holds no member",
            element: get("seed", "note.gap", Value::Null),
            reported: None,
        },
        Row {
            case: "several paths that all name nothing",
            element: get("seed", "absent, doc.gap, parts[9]", Value::Null),
            reported: None,
        },
        Row {
            case: "a key that holds no item",
            element: get("absent", "note", Value::Null),
            reported: None,
        },
        Row {
            case: "a key that holds no item, read with a projection naming nothing",
            element: get("absent", "absent", Value::Null),
            reported: None,
        },
        Row {
            case: "an attribute the item does hold",
            element: get("seed", "note", Value::Null),
            reported: Some(json!({"note": {"S": "hello"}})),
        },
        Row {
            case: "one path that matches beside two that do not",
            element: get("seed", "absent, tally, parts[9]", Value::Null),
            reported: Some(json!({"tally": {"N": "3"}})),
        },
        Row {
            case: "a path into a map that does match",
            element: get("seed", "doc.leaf", Value::Null),
            reported: Some(json!({"doc": {"M": {"leaf": {"S": "deep"}}}})),
        },
        Row {
            case: "a path at an index the stored list does hold",
            element: get("seed", "parts[1]", Value::Null),
            reported: Some(json!({"parts": {"L": [{"S": "one"}]}})),
        },
    ]
}

/// The `ItemResponse` at `at` of a served transaction.
fn response_at(answered: &Value, at: usize) -> Value {
    answered["Responses"][at].clone()
}

#[tokio::test]
async fn an_item_response_carries_an_item_only_where_the_get_projected_one() {
    let router = seeded().await;

    for row in rows() {
        let answered = ok(
            &router,
            "TransactGetItems",
            json!({"TransactItems": [row.element.clone()]}),
        )
        .await;
        assert_eq!(
            answered["Responses"].as_array().map(Vec::len),
            Some(1),
            "case {:?}: one ItemResponse per element, got {answered}",
            row.case
        );
        let response = response_at(&answered, 0);
        assert_eq!(
            response.get("Item").cloned(),
            row.reported,
            "case {:?}: expected Item {:?}, got {answered}",
            row.case,
            row.reported
        );
        // A get with nothing to report answers the EMPTY ItemResponse: no member at all,
        // rather than an `Item` with nothing in it.
        let expected_members = usize::from(row.reported.is_some());
        assert_eq!(
            response.as_object().map(serde_json::Map::len),
            Some(expected_members),
            "case {:?}: expected {expected_members} member(s) in the ItemResponse, got \
             {answered}",
            row.case
        );
    }
}

#[tokio::test]
async fn each_position_of_a_transaction_is_answered_on_its_own() {
    let router = seeded().await;

    // Every row read beside a get that DOES project something, in both orders: an
    // `ItemResponse` "corresponds to the TransactGetItem object in the same position", so one
    // element reporting nothing changes nothing about its neighbour.
    let matching = get("other", "note", Value::Null);
    for row in rows() {
        for (case, elements, at, other) in [
            (
                format!("{} first", row.case),
                vec![row.element.clone(), matching.clone()],
                0usize,
                1usize,
            ),
            (
                format!("{} last", row.case),
                vec![matching.clone(), row.element.clone()],
                1,
                0,
            ),
        ] {
            let answered = ok(
                &router,
                "TransactGetItems",
                json!({"TransactItems": elements}),
            )
            .await;
            assert_eq!(
                answered["Responses"].as_array().map(Vec::len),
                Some(2),
                "case {case:?}: one ItemResponse per element, got {answered}"
            );
            assert_eq!(
                response_at(&answered, at).get("Item").cloned(),
                row.reported,
                "case {case:?}: expected Item {:?} at {at}, got {answered}",
                row.reported
            );
            assert_eq!(
                response_at(&answered, other)["Item"],
                json!({"note": {"S": "hello"}}),
                "case {case:?}: the neighbouring get reports its own projection, got \
                 {answered}"
            );
        }
    }
}

#[tokio::test]
async fn the_transactional_reads_leave_the_stored_item_whole() {
    let router = seeded().await;

    for row in rows() {
        ok(
            &router,
            "TransactGetItems",
            json!({"TransactItems": [row.element]}),
        )
        .await;
    }
    // A read reports a projection; it never narrows what the store holds.
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
