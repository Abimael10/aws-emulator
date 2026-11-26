// SPDX-License-Identifier: CC0-1.0

//! Which ITEM an update's document paths are resolved against.
//!
//! RULE: a document path reaches into the item the update edits, so a path whose steps
//! before the last reach no container the last step can read — a parent that is not there,
//! a key missing under a map it passes through, a scalar where it needs a map, a `[]` index
//! where it needs a list — addresses no location OF THAT STORED ITEM, and the update is
//! refused 400 `ValidationException` with AWS's fixed sentence, "The document path provided
//! in the update expression is invalid for update", leaving the item exactly as it was:
//! never served as the no-op it would otherwise be, since a client relying on the refusal
//! to catch a stale path would be told its write succeeded.
//!
//! The sentence is FIXED: nothing of the path, of the clause or of the stored value is
//! appended to it.
//!
//! A key that holds NO ITEM has no stored image for a path to be invalid for: "adds a new
//! item to the table if it does not already exist" (`UpdateItem`), so those clauses are
//! applied to the item the update creates — one that only takes something away (`REMOVE`,
//! `DELETE`) has nothing to do and writes nothing, while one that WRITES still needs the
//! location it writes into.
//!
//! The rows span the clause kind (`SET`, `REMOVE`, `ADD`, `DELETE`), the SHAPE of the
//! defect (an absent top-level parent, a key missing under an existing map, a step through
//! a stored scalar, a step through a scalar list element, an index on a non-list, an index
//! past the end of a list in a non-last position), the form the path is written in (a bare
//! name and an `ExpressionAttributeNames` alias) and whether the addressed key holds an
//! item — plus the paths the same item DOES address, which must still apply.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "DocumentPaths";

/// AWS's fixed sentence for a path that addresses no location of the stored item.
const PATH_INVALID: &str =
    "The document path provided in the update expression is invalid for update";

/// The item every row is driven against: a map with one nested map, a list whose first
/// element is a map and whose second is a scalar, and a top-level scalar.
fn seeded_item(key: &str) -> Value {
    json!({
        "pk": {"S": key},
        "keep": {"S": "me"},
        "box": {"M": {"nest": {"M": {"leaf": {"S": "deep"}}}}},
        "slots": {"L": [{"M": {"child": {"S": "first"}}}, {"S": "second"}]},
    })
}

/// One row: the clause the update states, and the values it binds.
struct Row {
    case: &'static str,
    expression: &'static str,
    values: Value,
    names: Value,
}

/// The paths that address no location of the seeded item, one per shape and clause kind.
fn refused_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a REMOVE through an absent top-level parent",
            expression: "REMOVE absent.child",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE through a key missing under an existing map",
            expression: "REMOVE box.absent.leaf",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE through a stored scalar",
            expression: "REMOVE keep.child",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE through a scalar list element",
            expression: "REMOVE slots[1].child",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE indexing a map as a list",
            expression: "REMOVE box[0]",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE indexing a scalar as a list",
            expression: "REMOVE keep[0]",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE through an index past the end of a list",
            expression: "REMOVE slots[9].child",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a SET through an absent top-level parent",
            expression: "SET absent.child = :v",
            values: json!({":v": {"S": "x"}}),
            names: Value::Null,
        },
        Row {
            case: "a SET through a key missing under an existing map",
            expression: "SET box.absent.leaf = :v",
            values: json!({":v": {"S": "x"}}),
            names: Value::Null,
        },
        Row {
            case: "an ADD through an absent top-level parent",
            expression: "ADD absent.tally :one",
            values: json!({":one": {"N": "1"}}),
            names: Value::Null,
        },
        Row {
            case: "an ADD through a key missing under an existing map",
            expression: "ADD box.absent.tally :one",
            values: json!({":one": {"N": "1"}}),
            names: Value::Null,
        },
        Row {
            case: "a DELETE through an absent top-level parent",
            expression: "DELETE absent.tags :members",
            values: json!({":members": {"SS": ["a"]}}),
            names: Value::Null,
        },
        Row {
            case: "a DELETE through a key missing under an existing map",
            expression: "DELETE box.absent.tags :members",
            values: json!({":members": {"SS": ["a"]}}),
            names: Value::Null,
        },
        Row {
            // The same defect behind aliases: a path is judged on the location it
            // addresses once the names are resolved, not on how the text spells it.
            case: "a REMOVE through a missing segment written behind aliases",
            expression: "REMOVE #b.#a.#l",
            values: Value::Null,
            names: json!({"#b": "box", "#a": "absent", "#l": "leaf"}),
        },
    ]
}

/// The paths that DO address a location of the same item, so the rows above are the shape
/// being judged and not a blanket refusal.
fn served_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a REMOVE of a top-level attribute that is not there",
            expression: "REMOVE absent",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE of a key missing under an existing map",
            expression: "REMOVE box.absent",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE of an index past the end of a list",
            expression: "REMOVE slots[9]",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a REMOVE of a key the map holds",
            expression: "REMOVE box.nest.leaf",
            values: Value::Null,
            names: Value::Null,
        },
        Row {
            case: "a SET of a key missing under an existing map",
            expression: "SET box.fresh = :v",
            values: json!({":v": {"S": "x"}}),
            names: Value::Null,
        },
        Row {
            case: "a SET past the end of a list",
            expression: "SET slots[9] = :v",
            values: json!({":v": {"S": "x"}}),
            names: Value::Null,
        },
        Row {
            case: "an ADD of a key missing under an existing map",
            expression: "ADD box.tally :one",
            values: json!({":one": {"N": "1"}}),
            names: Value::Null,
        },
        Row {
            case: "a DELETE of a key missing under an existing map",
            expression: "DELETE box.tags :members",
            values: json!({":members": {"SS": ["a"]}}),
            names: Value::Null,
        },
    ]
}

/// One `UpdateItem` request, with the members a row states left out when it states none.
fn request(key: &Value, row: &Row) -> Value {
    let mut body = json!({
        "TableName": TABLE,
        "Key": key,
        "UpdateExpression": row.expression,
    });
    if !row.values.is_null() {
        body["ExpressionAttributeValues"] = row.values.clone();
    }
    if !row.names.is_null() {
        body["ExpressionAttributeNames"] = row.names.clone();
    }
    body
}

/// The item at `key`, or `Value::Null` when the key holds none.
async fn stored(router: &MultiServiceRouter, key: &Value) -> Value {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key, "ConsistentRead": true}),
    )
    .await;
    read.get("Item").cloned().unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_path_addressing_no_location_of_the_stored_item_is_refused() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    for (at, row) in refused_rows().iter().enumerate() {
        let name = format!("stored-{at}");
        let key = json!({"pk": {"S": &name}});
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": seeded_item(&name)}),
        )
        .await;
        let body = request(&key, row);
        let (status, answered) = call(&router, "UpdateItem", body.clone()).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", PATH_INVALID),
            "case {:?} ({body}): expected AWS's fixed path sentence, got {status} with \
             {answered}",
            row.case
        );
        assert_eq!(
            stored(&router, &key).await,
            seeded_item(&name),
            "case {:?}: a refused update leaves the item exactly as it was",
            row.case
        );
    }
}

#[tokio::test]
async fn a_path_the_stored_item_addresses_still_applies() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    for (at, row) in served_rows().iter().enumerate() {
        let name = format!("served-{at}");
        let key = json!({"pk": {"S": &name}});
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": seeded_item(&name)}),
        )
        .await;
        let body = request(&key, row);
        let (status, answered) = call(&router, "UpdateItem", body.clone()).await;
        assert_eq!(
            status, 200,
            "case {:?} ({body}): a path the item addresses applies, got {status} with \
             {answered}",
            row.case
        );
        // The item is still there and still carries what the clause did not touch.
        let item = stored(&router, &key).await;
        assert_eq!(
            item["keep"],
            json!({"S": "me"}),
            "case {:?}: expected the untouched attribute, got {item}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_key_that_holds_no_item_has_no_stored_image_to_resolve_a_path_against() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // A clause that only TAKES SOMETHING AWAY from a key that holds no item has nothing to
    // do: there is no stored image for its path to be invalid for, and nothing is written
    // — "adds a new item to the table if it does not already exist" adds the item only
    // where a clause gave it a value.
    for (at, row) in refused_rows()
        .iter()
        .filter(|row| row.expression.starts_with("REMOVE") || row.expression.starts_with("DELETE"))
        .enumerate()
    {
        let name = format!("fresh-{at}");
        let key = json!({"pk": {"S": &name}});
        let body = request(&key, row);
        let (status, answered) = call(&router, "UpdateItem", body.clone()).await;
        assert_eq!(
            status, 200,
            "case {:?} against a key that holds no item ({body}): expected it to be \
             served, got {status} with {answered}",
            row.case
        );
        assert_eq!(
            stored(&router, &key).await,
            Value::Null,
            "case {:?} against a key that holds no item: a clause that only removes adds \
             no item",
            row.case
        );
    }

    // A clause that WRITES still needs the location it writes into, even on a key that
    // holds no item: the item the update creates is built here, and a parent that is not
    // there is no location to write into — which is the other half of the rule.
    for (at, row) in refused_rows()
        .iter()
        .filter(|row| row.expression.starts_with("SET") || row.expression.starts_with("ADD"))
        .enumerate()
    {
        let name = format!("writes-{at}");
        let key = json!({"pk": {"S": &name}});
        let body = request(&key, row);
        let (status, answered) = call(&router, "UpdateItem", body.clone()).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", PATH_INVALID),
            "case {:?} against a key that holds no item ({body}): a clause that writes \
             still needs its location, got {status} with {answered}",
            row.case
        );
        assert_eq!(
            stored(&router, &key).await,
            Value::Null,
            "case {:?} against a key that holds no item: a refused write adds no item",
            row.case
        );
    }

    // The same key once an item IS stored under it: the very same clause is refused, which
    // is what makes the rule about the STORED image and not about the clause.
    let key = json!({"pk": {"S": "fresh-0"}});
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": seeded_item("fresh-0")}),
    )
    .await;
    let (status, answered) = call(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key, "UpdateExpression": "REMOVE absent.child"}),
    )
    .await;
    assert_eq!(
        (
            status,
            error_code(&answered).as_str(),
            message(&answered).as_str()
        ),
        (400, "ValidationException", PATH_INVALID),
        "case the same REMOVE once the key holds an item: expected the path refusal, got \
         {status} with {answered}"
    );
}
