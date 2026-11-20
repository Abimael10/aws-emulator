// SPDX-License-Identifier: CC0-1.0

//! What a read that reports an item in an `Item` MEMBER answers when the projection it
//! applied matched nothing, driven end to end.
//!
//! RULE under test: whether the key addresses a stored item is the only thing that decides
//! whether such a read reports the member at all — "if there is no matching item, GetItem
//! does not return any data and there will be no Item element in the response" (`GetItem`),
//! and "if a requested item could not be retrieved, the corresponding ItemResponse object is
//! Null" (`TransactGetItems$Responses`). The projection decides only the CONTENTS of what is
//! reported, so an item that WAS found reports the member whatever its projection matched:
//! a projection naming no location of that item reports the map with nothing in it that it
//! projected to ("if the requested item has no projected attributes, the corresponding
//! ItemResponse object is an empty Map" — the map of projected name-value pairs being the
//! empty one), never the absence that stands for "there is no such item".
//!
//! The rows span what the rule generalises over: the FORM of the projection that matched
//! nothing (an attribute the item does not hold, that attribute reached through an alias, a
//! path into a map the item holds nothing at, a path into a list past its end, a path
//! through a scalar, several such paths at once), the MEMBER the projection was stated
//! through (`ProjectionExpression` and the legacy `AttributesToGet`), and the item it was
//! applied to (one with attributes of every shape, and a key that holds no item at all) —
//! each beside a projection that DOES match, so the rows show what the member carries for
//! the same reader and the same item.
//!
//! The complement is pinned beside it: a reader that reports its items as a LIST carries an
//! element for an item that projected nothing too, so `Query`, `Scan` and the per-table list
//! of a `BatchGetItem` answer that element as the empty map.

use serde_json::{json, Value};

use crate::emulator::{emulator, ok};
use crate::table_items::create_active_table;

/// The table every row below reads: one string partition key.
const TABLE: &str = "ProjectedItemMember";

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

/// The key of that item.
fn seed_key() -> Value {
    json!({"pk": {"S": "seed"}})
}

/// A key the table holds no item for.
fn absent_key() -> Value {
    json!({"pk": {"S": "absent"}})
}

/// One row: the case, the key read, the projection, its aliases, and the `Item` the read
/// must report — `None` where the member must be ABSENT, which is only where the key holds
/// no item.
struct Row {
    case: &'static str,
    key: Value,
    expression: &'static str,
    names: Value,
    reported: Option<Value>,
}

/// The rows, over the projections that match nothing and the ones that do.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an attribute the item does not hold",
            key: seed_key(),
            expression: "absent",
            names: Value::Null,
            reported: Some(json!({})),
        },
        Row {
            case: "that attribute reached through an alias",
            key: seed_key(),
            expression: "#a",
            names: json!({"#a": "absent"}),
            reported: Some(json!({})),
        },
        Row {
            case: "a path into a map the item holds nothing at",
            key: seed_key(),
            expression: "doc.gap",
            names: Value::Null,
            reported: Some(json!({})),
        },
        Row {
            case: "a path past the end of a stored list",
            key: seed_key(),
            expression: "parts[9]",
            names: Value::Null,
            reported: Some(json!({})),
        },
        Row {
            case: "a path through a scalar, which holds no member",
            key: seed_key(),
            expression: "note.gap",
            names: Value::Null,
            reported: Some(json!({})),
        },
        Row {
            case: "several paths that all match nothing",
            key: seed_key(),
            expression: "absent, doc.gap, parts[9]",
            names: Value::Null,
            reported: Some(json!({})),
        },
        Row {
            case: "a key that holds no item, which reports NO member",
            key: absent_key(),
            expression: "note",
            names: Value::Null,
            reported: None,
        },
        Row {
            case: "a key that holds no item, read with a projection matching nothing",
            key: absent_key(),
            expression: "absent",
            names: Value::Null,
            reported: None,
        },
        Row {
            case: "an attribute the item does hold, which IS reported",
            key: seed_key(),
            expression: "note",
            names: Value::Null,
            reported: Some(json!({"note": {"S": "hello"}})),
        },
        Row {
            case: "one path that matches beside two that do not",
            key: seed_key(),
            expression: "absent, tally, parts[9]",
            names: Value::Null,
            reported: Some(json!({"tally": {"N": "3"}})),
        },
        Row {
            case: "a path into a map that does match",
            key: seed_key(),
            expression: "doc.leaf",
            names: Value::Null,
            reported: Some(json!({"doc": {"M": {"leaf": {"S": "deep"}}}})),
        },
    ]
}

/// The emulator with that one table and that one item in it.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
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

/// A `GetItem` body for `row`.
fn get_body(row: &Row) -> Value {
    let mut body = json!({
        "TableName": TABLE,
        "Key": row.key.clone(),
        "ProjectionExpression": row.expression,
    });
    if !row.names.is_null() {
        body["ExpressionAttributeNames"] = row.names.clone();
    }
    body
}

/// A `TransactGetItems` body carrying one `Get` for `row`.
fn transact_body(row: &Row) -> Value {
    let mut element = json!({
        "TableName": TABLE,
        "Key": row.key.clone(),
        "ProjectionExpression": row.expression,
    });
    if !row.names.is_null() {
        element["ExpressionAttributeNames"] = row.names.clone();
    }
    json!({"TransactItems": [{"Get": element}]})
}

#[tokio::test]
async fn a_single_item_read_reports_the_item_member_of_every_item_it_found() {
    let router = seeded().await;

    for row in rows() {
        let read = ok(&router, "GetItem", get_body(&row)).await;
        let reported = read.get("Item").cloned();
        assert_eq!(
            reported, row.reported,
            "case {:?} (GetItem): expected {:?}, got {read}",
            row.case, row.reported
        );
        // The member is absent only where there was no item to report.
        assert_eq!(
            reported.is_none(),
            row.reported.is_none(),
            "case {:?} (GetItem): only a key holding no item reports no member, got {read}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_legacy_projection_member_reports_that_item_the_same_way() {
    let router = seeded().await;

    // `AttributesToGet` is "a legacy parameter. Use ProjectionExpression instead" (that
    // member's doc) — the same ask one generation apart, so what the read REPORTS of the
    // item it found follows the same rule: a list naming only attributes the item does not
    // hold still reports the item it found, as the empty map.
    for (case, key, names, expected) in [
        (
            "a name the item does not hold",
            seed_key(),
            vec!["absent"],
            Some(json!({})),
        ),
        (
            "several names the item does not hold",
            seed_key(),
            vec!["absent", "missing"],
            Some(json!({})),
        ),
        (
            "a name the item holds beside one it does not",
            seed_key(),
            vec!["absent", "note"],
            Some(json!({"note": {"S": "hello"}})),
        ),
        (
            "a name whose value is a whole nested document",
            seed_key(),
            vec!["doc"],
            Some(json!({"doc": {"M": {"leaf": {"S": "deep"}}}})),
        ),
        (
            "a name whose value is a whole stored list",
            seed_key(),
            vec!["parts"],
            Some(json!({"parts": {"L": [{"S": "zero"}, {"S": "one"}]}})),
        ),
        (
            "a key that holds no item",
            absent_key(),
            vec!["absent"],
            None,
        ),
    ] {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": key, "AttributesToGet": names}),
        )
        .await;
        assert_eq!(
            read.get("Item").cloned(),
            expected,
            "case {case:?}: expected Item {expected:?}, got {read}"
        );
    }
}

#[tokio::test]
async fn an_element_of_a_transactional_read_reports_it_the_same_way() {
    let router = seeded().await;

    for row in rows() {
        let read = ok(&router, "TransactGetItems", transact_body(&row)).await;
        assert_eq!(
            read["Responses"].as_array().map(Vec::len),
            Some(1),
            "case {:?}: one ItemResponse per element, got {read}",
            row.case
        );
        let reported = read["Responses"][0].get("Item").cloned();
        assert_eq!(
            reported, row.reported,
            "case {:?} (TransactGetItems): expected {:?}, got {read}",
            row.case, row.reported
        );
        assert_eq!(
            reported.is_none(),
            row.reported.is_none(),
            "case {:?} (TransactGetItems): only a get that retrieved no item reports no \
             member, got {read}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_projected_reads_leave_the_stored_item_whole() {
    let router = seeded().await;

    for row in rows() {
        ok(&router, "GetItem", get_body(&row)).await;
        ok(&router, "TransactGetItems", transact_body(&row)).await;
    }
    // A read reports a projection; it never narrows what the store holds.
    let whole = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": seed_key()}),
    )
    .await;
    assert_eq!(
        whole["Item"],
        stored_item(),
        "the projected reads must leave the stored item whole, got {whole}"
    );
}

#[tokio::test]
async fn a_reader_that_reports_a_list_carries_an_element_that_projected_nothing_too() {
    let router = seeded().await;

    // An item whose projection matched nothing is still an element of the list a reader
    // reports its items in, answered as the empty map it projected to.
    let listed: [(&str, Value); 3] = [
        (
            "Query",
            json!({"TableName": TABLE, "ProjectionExpression": "absent",
                   "KeyConditionExpression": "pk = :p",
                   "ExpressionAttributeValues": {":p": {"S": "seed"}}}),
        ),
        (
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "absent"}),
        ),
        (
            "BatchGetItem",
            json!({"RequestItems": {TABLE: {"Keys": [seed_key()],
                                            "ProjectionExpression": "absent"}}}),
        ),
    ];

    for (operation, body) in listed {
        let answered = ok(&router, operation, body).await;
        let elements = match operation {
            "BatchGetItem" => answered["Responses"][TABLE].clone(),
            _ => answered["Items"].clone(),
        };
        assert_eq!(
            elements,
            json!([{}]),
            "case {operation}: one element that projected nothing, got {answered}"
        );
    }
}
