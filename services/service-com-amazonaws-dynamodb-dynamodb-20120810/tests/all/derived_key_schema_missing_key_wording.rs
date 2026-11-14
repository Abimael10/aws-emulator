// SPDX-License-Identifier: CC0-1.0

//! WHICH key attribute a single-item write is told it left out, for a key schema whose
//! attribute names are decided at RUNTIME rather than written into the client.
//!
//! RULE — a write that carries an `Item` (the whole item, not a `Key`) and gives no value
//! to a key attribute the table's `KeySchema` declares is refused HTTP 400
//! `ValidationException` whose message NAMES that attribute: "One or more parameter values
//! were invalid: Missing the key <name> in the item", the fixed clause every refusal of a
//! parameter value opens with over the fixed sentence for that defect, with the one
//! variable part taken from the table's own key schema. The name is whatever the schema
//! declares — a client that derived the schema at runtime (from a header row, say) learns
//! which column its item is missing and can repair it. When the item gives no value to
//! either key of a composite schema, the key named is the first element the schema
//! declares, which `CreateTable` holds to be the `HASH` one.
//!
//! The rows below span what the rule generalises over: the attribute's NAME (words,
//! underscores, dashes, dots, digits), the scalar TYPE it was defined as (`S`, `N`, `B`),
//! the ROLE it plays (`HASH`, `RANGE`), the schema's ARITY (one key attribute and two),
//! and WHICH declared key the item left out (the sort key, the partition key, both). Every
//! row then writes the COMPLETE item and reads it back, so a refusal is known to be about
//! the key the item omitted rather than about the write.
//!
//! The second suite holds the OTHER single-item write surfaces to the same sentence: a
//! PartiQL `INSERT` carries an item too, over `ExecuteStatement` and inside one slot of
//! `BatchExecuteStatement`, and reads its item's key through the same walk over the
//! schema.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, ok};
use crate::probe::refusal;

/// The clause every refusal of a parameter value in this service opens with.
const INVALID_PARAMETER: &str = "One or more parameter values were invalid";

/// The sentence that refusal states for a key attribute an `Item` gave no value for,
/// naming the attribute the table's key schema declares.
fn missing_the_key(attribute: &str) -> String {
    format!("{INVALID_PARAMETER}: Missing the key {attribute} in the item")
}

/// One key attribute as a client that derived the schema declares it: the name it read,
/// the role it gives it, and the scalar type it defined for it.
type DeclaredKey = (&'static str, &'static str, &'static str);

/// One case: a derived key schema, the declared keys the item gives no value for, and the
/// attribute the refusal must name.
struct Row {
    case: &'static str,
    table: &'static str,
    keys: Vec<DeclaredKey>,
    omitted: Vec<&'static str>,
    named: &'static str,
}

/// Create a table from `keys`, as `(attribute, key type, attribute type)` rows, and wait
/// out the `CREATING` -> `ACTIVE` transition by POLLING the description.
async fn create_table_with_keys(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    keys: &[DeclaredKey],
) {
    let key_schema: Vec<Value> = keys
        .iter()
        .map(|(attribute, role, _)| json!({"AttributeName": attribute, "KeyType": role}))
        .collect();
    let definitions: Vec<Value> = keys
        .iter()
        .map(|(attribute, _, declared)| {
            json!({"AttributeName": attribute, "AttributeType": declared})
        })
        .collect();
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": key_schema,
            "AttributeDefinitions": definitions,
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, case, name).await;
}

/// A value of the scalar type `declared`, so every key the item DOES carry is of the type
/// the schema declared for it and the refusal can only be about the key it left out.
fn value_of(declared: &str) -> Value {
    match declared {
        "N" => json!({"N": "7"}),
        // `B` is base64 on the wire, as `AttributeValue$B` declares.
        "B" => json!({"B": "aGk="}),
        _ => json!({"S": "derived"}),
    }
}

/// The item a write carries: a value for every declared key EXCEPT the ones in `omitted`,
/// plus one attribute the schema declares no key for (the item's own business).
fn item_without(keys: &[DeclaredKey], omitted: &[&str]) -> Value {
    let mut item = serde_json::Map::new();
    for (attribute, _, declared) in keys {
        if !omitted.contains(attribute) {
            item.insert((*attribute).to_string(), value_of(declared));
        }
    }
    item.insert("payload".to_string(), json!({"S": "row"}));
    Value::Object(item)
}

/// The whole item: every declared key with a value of its declared type.
fn complete_item(keys: &[DeclaredKey]) -> Value {
    item_without(keys, &[])
}

/// The `Key` that addresses [`complete_item`]'s item.
fn complete_key(keys: &[DeclaredKey]) -> Value {
    let mut key = serde_json::Map::new();
    for (attribute, _, declared) in keys {
        key.insert((*attribute).to_string(), value_of(declared));
    }
    Value::Object(key)
}

#[tokio::test]
async fn a_put_names_the_declared_key_its_item_gave_no_value_for() {
    let router = emulator();

    // The rows: a key schema a client derived, the key attributes its item gives no value
    // for, and the attribute the refusal must name.
    let rows: Vec<Row> = vec![
        Row {
            case: "a two-column header, no value for the sort column",
            table: "InventoryWords",
            keys: vec![("item_id", "HASH", "S"), ("warehouse_id", "RANGE", "S")],
            omitted: vec!["warehouse_id"],
            named: "warehouse_id",
        },
        Row {
            case: "the same schema, no value for the partition column",
            table: "InventoryWords",
            keys: vec![("item_id", "HASH", "S"), ("warehouse_id", "RANGE", "S")],
            omitted: vec!["item_id"],
            named: "item_id",
        },
        Row {
            case: "the same schema, no value for either column",
            table: "InventoryWords",
            keys: vec![("item_id", "HASH", "S"), ("warehouse_id", "RANGE", "S")],
            omitted: vec!["item_id", "warehouse_id"],
            named: "item_id",
        },
        Row {
            case: "a numeric sort column",
            table: "ReceiptsNumeric",
            keys: vec![("sku", "HASH", "S"), ("received_at", "RANGE", "N")],
            omitted: vec!["received_at"],
            named: "received_at",
        },
        Row {
            case: "a binary partition column",
            table: "PartsBinary",
            keys: vec![("checksum", "HASH", "B"), ("part", "RANGE", "S")],
            omitted: vec!["checksum"],
            named: "checksum",
        },
        Row {
            case: "columns spelled with a dash, a dot and digits",
            table: "PunctuatedColumns",
            keys: vec![("order-id", "HASH", "S"), ("Line.Item_2", "RANGE", "N")],
            omitted: vec!["Line.Item_2"],
            named: "Line.Item_2",
        },
        Row {
            case: "columns named by their position in the header",
            table: "PositionalColumns",
            keys: vec![("1", "HASH", "N"), ("2", "RANGE", "S")],
            omitted: vec!["1"],
            named: "1",
        },
        Row {
            case: "a one-column header: the only key",
            table: "SingleColumn",
            keys: vec![("row", "HASH", "N")],
            omitted: vec!["row"],
            named: "row",
        },
    ];

    for row in &rows {
        // The same table serves several rows; create it once.
        let (status, _) = call(&router, "DescribeTable", json!({"TableName": row.table})).await;
        if status != 200 {
            create_table_with_keys(&router, row.case, row.table, &row.keys).await;
        }

        let stated = refusal(
            &router,
            row.case,
            "PutItem",
            json!({
                "TableName": row.table,
                "Item": item_without(&row.keys, &row.omitted),
            }),
        )
        .await;
        let expected = missing_the_key(row.named);
        let case = row.case;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // Every schema above accepts the item that gives each declared key a value, and reads
    // it back: the rows are about the key their item omitted, not about the write.
    for row in &rows {
        ok(
            &router,
            "PutItem",
            json!({"TableName": row.table, "Item": complete_item(&row.keys)}),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": row.table, "Key": complete_key(&row.keys)}),
        )
        .await;
        let case = row.case;
        assert_eq!(
            read["Item"],
            complete_item(&row.keys),
            "case {case:?}: expected the complete item back, got {read}"
        );
    }
}

#[tokio::test]
async fn a_partiql_insert_names_the_declared_key_its_item_gave_no_value_for() {
    let router = emulator();
    let keys: Vec<DeclaredKey> = vec![("item_id", "HASH", "S"), ("warehouse_id", "RANGE", "S")];
    create_table_with_keys(&router, "a derived composite key", "inventory", &keys).await;

    // Each row: the case, the statement, and the key the refusal must name. An `INSERT`
    // carries an ITEM, so the rows span which declared key that item gives no value for.
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "an insert with no value for the sort column",
            "INSERT INTO inventory VALUE {'item_id': 'sku-1', 'payload': 'row'}",
            "warehouse_id",
        ),
        (
            "an insert with no value for the partition column",
            "INSERT INTO inventory VALUE {'warehouse_id': 'w-1', 'payload': 'row'}",
            "item_id",
        ),
        (
            "an insert with no value for either column",
            "INSERT INTO inventory VALUE {'payload': 'row'}",
            "item_id",
        ),
    ];

    for (case, statement, named) in &rows {
        let stated = refusal(
            &router,
            case,
            "ExecuteStatement",
            json!({"Statement": statement}),
        )
        .await;
        let expected = missing_the_key(named);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // The same statement in ONE SLOT of a batch is reported in that slot, which states
    // the batch's own code for a refused statement and the same sentence.
    let answered = ok(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": rows
            .iter()
            .map(|(_, statement, _)| json!({"Statement": statement}))
            .collect::<Vec<Value>>()}),
    )
    .await;
    let slots = answered["Responses"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        slots.len(),
        rows.len(),
        "case a batch of inserts missing a key: expected one slot per statement, got {answered}"
    );
    for ((case, _, named), slot) in rows.iter().zip(&slots) {
        assert_eq!(
            (
                slot["Error"]["Code"].as_str().unwrap_or("<no Code>"),
                slot["Error"]["Message"].as_str().unwrap_or("<no Message>"),
            ),
            ("ValidationError", missing_the_key(named).as_str()),
            "case {case:?}, batched: expected the slot to name {named:?}, got {slot}"
        );
    }

    // An insert that gives both declared keys a value is written, and the item is there to
    // read: the refusals above are about the key the statement's item omitted.
    ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": "INSERT INTO inventory VALUE {'item_id': 'sku-1', \
                             'warehouse_id': 'w-1', 'payload': 'row'}"}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({
            "TableName": "inventory",
            "Key": {"item_id": {"S": "sku-1"}, "warehouse_id": {"S": "w-1"}},
        }),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({
            "item_id": {"S": "sku-1"},
            "warehouse_id": {"S": "w-1"},
            "payload": {"S": "row"},
        }),
        "case a complete insert: expected the inserted item back, got {read}"
    );
}
