// SPDX-License-Identifier: CC0-1.0

//! A table whose key schema is DERIVED from an object in Amazon S3, and WHICH key attribute a
//! single-item write is told its item gave no value for.
//!
//! RULE: a write that carries an `Item` (the whole item, not a `Key`) and gives no value to a
//! key attribute the TABLE's `KeySchema` declares is refused HTTP 400 `ValidationException`
//! whose message NAMES that attribute — "One or more parameter values were invalid: Missing
//! the key <name> in the item", the clause every refusal of a parameter value opens with over
//! the fixed sentence for that defect, with the one variable part taken from the table's own
//! key schema and the submitted item. The table's key schema is the WHOLE of what an item must
//! carry: a key attribute of a secondary index is required only to be of its declared type
//! WHEN the item carries it, so an item that gives no value to one is written (it is simply
//! not indexed), and when an item gives no value to either key of a composite schema the name
//! stated is the first element the schema declares, which `CreateTable` holds to be the `HASH`
//! one.
//!
//! The attribute name is the entire value of that refusal, and it is worth something only
//! where the client does not already know it: here the names, their roles and their scalar
//! types are read out of a CSV object's header and first data row, so nothing about the schema
//! is written into the caller — the object decides it, as a client that keys a table per
//! uploaded object does. Amazon S3 is mounted beside this service on one engine
//! ([`emulator_with_s3`]) because the object the schema is derived from is that service's
//! resource: it exists only if S3 serves it back.
//!
//! The rows span what the rule generalises over: the NAMES the header row declares (words with
//! underscores, a dash, a dot and digits, a bare ordinal), the scalar TYPE each column's data
//! implies (`S` and `N`), the schema's ARITY (one key column and two), the ROLE of the key the
//! item leaves out (`RANGE`, `HASH`), how many it leaves out (one, both), and whether the table
//! carries secondary indexes whose own key attributes the item gives no value for. Every row
//! then writes the COMPLETE item the same object implies and reads it back through `GetItem`,
//! so a refusal is known to be about the key the item omitted rather than about the write.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{create_bucket, describe_when_active, emulator_with_s3, ok, put_object, s3};
use crate::probe::refusal;

/// The clause every refusal of a parameter value in this service opens with.
const INVALID_PARAMETER: &str = "One or more parameter values were invalid";

/// The bucket every CSV object below is stored in.
const BUCKET: &str = "derived-schema-landing";

/// The sentence a single-item write states for a key attribute its `Item` gave no value for,
/// naming the attribute the table's key schema declares.
fn missing_the_key(attribute: &str) -> String {
    format!("{INVALID_PARAMETER}: Missing the key {attribute} in the item")
}

/// One column as a CSV object's header row and first data row declare it.
#[derive(Clone, Debug)]
struct Column {
    /// The name the header row gives it, which is the name a refusal must state.
    name: String,
    /// The scalar type its datum is expressible as.
    declared: String,
    /// The datum itself, which the item written for this column carries.
    value: String,
}

/// One column's datum as an `AttributeValue`: the scalar type its data implied, over the datum.
fn attribute_value(column: &Column) -> Value {
    let mut value = Map::new();
    value.insert(
        column.declared.clone(),
        Value::String(column.value.clone()),
    );
    Value::Object(value)
}

/// The table one CSV object implies: its columns in header order, the first two of which are
/// its partition and sort key.
#[derive(Clone, Debug)]
struct Derived {
    columns: Vec<Column>,
}

impl Derived {
    /// The columns that play a key role in the table: the first is the partition key, the
    /// second — when the header declares one — the sort key.
    fn keys(&self) -> &[Column] {
        &self.columns[..self.columns.len().min(2)]
    }

    /// The columns the table's key schema does NOT declare.
    fn spare(&self) -> &[Column] {
        &self.columns[self.columns.len().min(2)..]
    }

    /// `KeySchema`, in header order.
    fn key_schema(&self) -> Value {
        Value::Array(
            self.keys()
                .iter()
                .zip(["HASH", "RANGE"])
                .map(|(column, role)| json!({"AttributeName": column.name, "KeyType": role}))
                .collect(),
        )
    }

    /// `AttributeDefinitions` for every attribute a key schema here names — the table's keys,
    /// and the spare column a secondary index is keyed on when the row asks for one.
    fn attribute_definitions(&self, indexed: bool) -> Value {
        let named = if indexed {
            self.columns.len()
        } else {
            self.keys().len()
        };
        Value::Array(
            self.columns[..named]
                .iter()
                .map(|column| {
                    json!({"AttributeName": column.name, "AttributeType": column.declared})
                })
                .collect(),
        )
    }

    /// A global secondary index keyed on the FIRST spare column, if the object has one: an
    /// index whose key attribute is no key of the table, so an item that gives it no value is
    /// the table's business and not the index's.
    fn global_secondary_indexes(&self) -> Option<Value> {
        let spare = self.spare().first()?;
        Some(json!([{
            "IndexName": "by-spare",
            "KeySchema": [{"AttributeName": spare.name, "KeyType": "HASH"}],
            "Projection": {"ProjectionType": "ALL"},
        }]))
    }

    /// A local secondary index over the table's partition key and the SECOND spare column as its
    /// sort key, if the object declares one — so each index here is keyed on a column of its own,
    /// and a refusal about one of them names that index.
    fn local_secondary_indexes(&self) -> Option<Value> {
        let partition = self.keys().first()?;
        let spare = self.spare().get(1)?;
        Some(json!([{
            "IndexName": "by-partition-and-spare",
            "KeySchema": [
                {"AttributeName": partition.name, "KeyType": "HASH"},
                {"AttributeName": spare.name, "KeyType": "RANGE"},
            ],
            "Projection": {"ProjectionType": "KEYS_ONLY"},
        }]))
    }

    /// The item the object's data row is, with the columns named in `omitted` left out.
    fn item_without(&self, omitted: &[&str]) -> Value {
        let mut item = Map::new();
        for column in &self.columns {
            if !omitted.contains(&column.name.as_str()) {
                item.insert(column.name.clone(), attribute_value(column));
            }
        }
        Value::Object(item)
    }

    /// The `Key` that addresses the item `item_without` builds.
    fn key(&self) -> Value {
        let mut key = Map::new();
        for column in self.keys() {
            key.insert(column.name.clone(), attribute_value(column));
        }
        Value::Object(key)
    }
}

/// Read a CSV object's header row and first data row into the table they imply.
///
/// The names are the header row's cells, in order; each column's scalar type is the one its
/// datum is expressible as — a run of ASCII digits is this service's `N`, anything else its
/// `S` — so neither the names nor the types are written into the caller.
fn derive(csv: &str) -> Derived {
    let mut lines = csv.lines();
    let header = lines.next().unwrap_or_default();
    let data = lines.next().unwrap_or_default();
    let mut cells = data.split(',');
    let columns = header
        .split(',')
        .map(|name| {
            let value = cells.next().unwrap_or("0").trim().to_string();
            let numeric = !value.is_empty() && value.chars().all(|cell| cell.is_ascii_digit());
            Column {
                name: name.trim().to_string(),
                declared: if numeric { "N" } else { "S" }.to_string(),
                value,
            }
        })
        .collect();
    Derived { columns }
}

/// The table name a client derives from the object's key, so no row's table name says anything
/// about the schema inside it.
fn table_of(key: &str) -> String {
    format!("derived-{}", key.strip_suffix(".csv").unwrap_or(key))
}

/// One row: the CSV object the schema is derived from, whether the table carries secondary
/// indexes over the columns the key schema does not declare, the key columns the write's item
/// gives no value for, and the attribute the refusal must name.
struct Row {
    case: &'static str,
    key: &'static str,
    csv: &'static str,
    indexed: bool,
    omitted: &'static [&'static str],
    named: &'static str,
}

/// Store one CSV object in Amazon S3 and read it back from there: what the schema is derived
/// from is what that service serves, not what this suite happens to hold.
async fn stored_csv(router: &MultiServiceRouter, case: &str, key: &str, csv: &str) -> String {
    put_object(router, BUCKET, key, csv.as_bytes().to_vec()).await;
    let answered = s3(router)
        .get_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: S3 serves {key:?} back: {error:?}"));
    let bytes = answered
        .body
        .collect()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the object's bytes are readable: {error:?}"))
        .into_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Create the table `csv` implies, with the secondary indexes the row asked for, and wait out
/// the documented `CREATING` -> `ACTIVE` transition by POLLING its description.
async fn create_derived_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    derived: &Derived,
    indexed: bool,
) {
    let mut request = Map::new();
    request.insert("TableName".to_string(), json!(table));
    request.insert("KeySchema".to_string(), derived.key_schema());
    request.insert(
        "AttributeDefinitions".to_string(),
        derived.attribute_definitions(indexed),
    );
    request.insert("BillingMode".to_string(), json!("PAY_PER_REQUEST"));
    if indexed {
        if let Some(indexes) = derived.global_secondary_indexes() {
            request.insert("GlobalSecondaryIndexes".to_string(), indexes);
        }
        if let Some(indexes) = derived.local_secondary_indexes() {
            request.insert("LocalSecondaryIndexes".to_string(), indexes);
        }
    }
    ok(router, "CreateTable", Value::Object(request)).await;
    describe_when_active(router, case, table).await;
}

/// The rows: each a CSV object a client uploaded, and the key column its item leaves out.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a two-column header, no value for the sort column",
            key: "inventory.csv",
            csv: "item_id,warehouse_id\nsku-1,w-1\n",
            indexed: false,
            omitted: &["warehouse_id"],
            named: "warehouse_id",
        },
        Row {
            case: "the same object, no value for the partition column",
            key: "inventory.csv",
            csv: "item_id,warehouse_id\nsku-1,w-1\n",
            indexed: false,
            omitted: &["item_id"],
            named: "item_id",
        },
        Row {
            case: "the same object, no value for either column",
            key: "inventory.csv",
            csv: "item_id,warehouse_id\nsku-1,w-1\n",
            indexed: false,
            omitted: &["item_id", "warehouse_id"],
            named: "item_id",
        },
        Row {
            case: "a numeric sort column",
            key: "receipts.csv",
            csv: "sku,received_at\nabc-9,1700000000\n",
            indexed: false,
            omitted: &["received_at"],
            named: "received_at",
        },
        Row {
            case: "a dashed partition column and a dotted sort column, under an index",
            key: "shipments.csv",
            csv: "order-id,Line.Item_2,qty\no-1,7,3\n",
            indexed: true,
            omitted: &["Line.Item_2"],
            named: "Line.Item_2",
        },
        Row {
            case: "the same object, no value for the dashed partition column",
            key: "shipments.csv",
            csv: "order-id,Line.Item_2,qty\no-1,7,3\n",
            indexed: true,
            omitted: &["order-id"],
            named: "order-id",
        },
        Row {
            case: "a one-column header named by its position: the only key, numeric",
            key: "1.csv",
            csv: "1\n7\n",
            indexed: false,
            omitted: &["1"],
            named: "1",
        },
    ]
}

/// Every row's object stored, read back and turned into its table, each table created once.
async fn derived_tables(router: &MultiServiceRouter, rows: &[Row]) -> Vec<Derived> {
    let mut schemas: Vec<Derived> = Vec::new();
    let mut created: Vec<String> = Vec::new();
    for row in rows {
        let stored = stored_csv(router, row.case, row.key, row.csv).await;
        assert_eq!(
            stored, row.csv,
            "case {:?}: expected S3 to serve the stored CSV back, got {stored:?}",
            row.case
        );
        let derived = derive(&stored);
        let table = table_of(row.key);
        if !created.contains(&table) {
            create_derived_table(router, row.case, &table, &derived, row.indexed).await;
            created.push(table);
        }
        schemas.push(derived);
    }
    schemas
}

#[tokio::test]
async fn a_write_names_the_key_column_its_s3_derived_schema_declared() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;
    let rows = rows();
    let schemas = derived_tables(&router, &rows).await;

    // Every derived schema accepts the COMPLETE item the same object's data row is, and reads it
    // back, FIRST: a refusal below is then known to be about the key its item omitted rather than
    // about the write, and the stored item is there for the refusals to leave alone.
    for (row, derived) in rows.iter().zip(&schemas) {
        let table = table_of(row.key);
        let case = row.case;
        ok(
            &router,
            "PutItem",
            json!({"TableName": table, "Item": derived.item_without(&[])}),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": table, "Key": derived.key()}),
        )
        .await;
        assert_eq!(
            read["Item"],
            derived.item_without(&[]),
            "case {case:?}: expected the complete derived item back, got {read}"
        );
    }

    for (row, derived) in rows.iter().zip(&schemas) {
        let table = table_of(row.key);
        let case = row.case;
        let stated = refusal(
            &router,
            case,
            "PutItem",
            json!({"TableName": table, "Item": derived.item_without(row.omitted)}),
        )
        .await;
        let expected = missing_the_key(row.named);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );

        // And it stored nothing: the complete item written above is the one the table still
        // holds, so a refused put replaced no part of it.
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": table, "Key": derived.key()}),
        )
        .await;
        assert_eq!(
            read["Item"],
            derived.item_without(&[]),
            "case {case:?}: a refused put must leave the stored item alone, got {read}"
        );
    }
}

#[tokio::test]
async fn an_item_that_gives_a_secondary_index_key_no_value_is_written() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;

    // One object whose two spare columns key a global and a local secondary index: the index
    // keys are no part of what an item must carry, so an item that gives them no value is stored.
    let case = "an item carrying the table's keys and neither index key";
    let key = "pallets.csv";
    let csv = "order-id,Line.Item_2,qty,lot\no-1,7,3,L9\n";
    let stored = stored_csv(&router, case, key, csv).await;
    let derived = derive(&stored);
    let table = table_of(key);
    create_derived_table(&router, case, &table, &derived, true).await;

    // Every column the key schema does not declare, left out — which is every index key here.
    let spare: Vec<&str> = derived
        .spare()
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    assert!(
        !spare.is_empty(),
        "case {case:?}: the object must declare a column that is no key of the table, got \
         {derived:?}"
    );
    let item = derived.item_without(&spare);
    ok(
        &router,
        "PutItem",
        json!({"TableName": table, "Item": item.clone()}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": table, "Key": derived.key()}),
    )
    .await;
    assert_eq!(
        read["Item"], item,
        "case {case:?}: expected the item back as written, got {read}"
    );

    // And the index's own key attribute is still held to its declared TYPE when the item DOES
    // carry it: "If you specify any attributes that are part of an index key, then the data types
    // for those attributes must match those of the schema in the table's attribute definition"
    // (the `PutItem$Item` doc), stated as the mismatch between the two types for the index whose
    // key schema names the attribute.
    let mistyped_case = "an index key carried with another type than the table declares";
    let mut mistyped = item.as_object().cloned().unwrap_or_default();
    let indexed = derived.spare()[0].clone();
    let other = if indexed.declared == "N" { "S" } else { "N" };
    mistyped.insert(indexed.name.clone(), json!({other: "4"}));
    let stated = refusal(
        &router,
        mistyped_case,
        "PutItem",
        json!({"TableName": table, "Item": Value::Object(mistyped)}),
    )
    .await;
    let expected = format!(
        "{INVALID_PARAMETER}: Type mismatch for Index Key {} Expected: {} Actual: {other} \
         IndexName: by-spare",
        indexed.name, indexed.declared
    );
    assert_eq!(
        stated, expected,
        "case {mistyped_case:?}: expected {expected:?}, got {stated:?}"
    );

    // And it stored nothing over the item the table already held.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": table, "Key": derived.key()}),
    )
    .await;
    assert_eq!(
        read["Item"], item,
        "case {mistyped_case:?}: a refused put must leave the stored item alone, got {read}"
    );
}
