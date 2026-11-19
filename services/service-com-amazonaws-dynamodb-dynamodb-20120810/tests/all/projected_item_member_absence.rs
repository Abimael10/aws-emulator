// SPDX-License-Identifier: CC0-1.0

//! What a read that reports an item in an `Item` MEMBER answers when the projection it
//! applied matched nothing, driven end to end.
//!
//! RULE under test: an optional `Item` member is reported only when there is an attribute
//! to report — "if the requested item has no projected attributes, the corresponding
//! ItemResponse object is an empty Map" (`TransactGetItems$Responses`), so the RESPONSE
//! object is the empty one and the member it would have carried is absent. A projection
//! that matches no location of the item it found therefore reports NO member at all,
//! exactly as a key that holds no item does, and a client that tests for the member reads
//! "nothing was projected" rather than finding a map with nothing in it.
//!
//! The rows span what the rule generalises over: the READER that carries such a member
//! (`GetItem` and each element of a `TransactGetItems`), the FORM of the projection that
//! matched nothing (an attribute the item does not hold, that attribute reached through an
//! alias, a path into a map the item holds nothing at, a path into a list past its end, a
//! path through a scalar, several such paths at once), and the item the projection was
//! applied to (one with attributes of every shape, and a key that holds no item at all) —
//! each beside a projection that DOES match, so the rows show the member present and
//! absent for the same reader and the same item.
//!
//! The complement is pinned beside it: a reader that reports its items as a LIST has no
//! member to leave out, so `Query`, `Scan` and the per-table list of a `BatchGetItem`
//! still carry an element for an item that projected nothing.

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
/// must report — `None` where the member must be ABSENT.
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
            reported: None,
        },
        Row {
            case: "that attribute reached through an alias",
            key: seed_key(),
            expression: "#a",
            names: json!({"#a": "absent"}),
            reported: None,
        },
        Row {
            case: "a path into a map the item holds nothing at",
            key: seed_key(),
            expression: "doc.gap",
            names: Value::Null,
            reported: None,
        },
        Row {
            case: "a path past the end of a stored list",
            key: seed_key(),
            expression: "parts[9]",
            names: Value::Null,
            reported: None,
        },
        Row {
            case: "a path through a scalar, which holds no member",
            key: seed_key(),
            expression: "note.gap",
            names: Value::Null,
            reported: None,
        },
        Row {
            case: "several paths that all match nothing",
            key: seed_key(),
            expression: "absent, doc.gap, parts[9]",
            names: Value::Null,
            reported: None,
        },
        Row {
            case: "a key that holds no item, which reports no member either",
            key: absent_key(),
            expression: "note",
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
async fn a_single_item_read_reports_no_item_member_when_its_projection_matched_nothing() {
    let router = seeded().await;

    for row in rows() {
        let read = ok(&router, "GetItem", get_body(&row)).await;
        let reported = read.get("Item").cloned();
        assert_eq!(
            reported, row.reported,
            "case {:?} (GetItem): expected {:?}, got {read}",
            row.case, row.reported
        );
        assert_ne!(
            reported,
            Some(json!({})),
            "case {:?} (GetItem): an empty map is never reported, got {read}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_element_of_a_transactional_read_reports_no_item_member_either() {
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
        assert_ne!(
            reported,
            Some(json!({})),
            "case {:?} (TransactGetItems): an empty map is never reported, got {read}",
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
async fn a_reader_that_reports_a_list_still_carries_an_element_that_projected_nothing() {
    let router = seeded().await;

    // The readers that report their items as a LIST have no member to omit, so an item
    // whose projection matched nothing is still an element of that list.
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
