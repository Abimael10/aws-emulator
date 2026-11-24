// SPDX-License-Identifier: CC0-1.0

//! What a `SearchVectors` result carries of the VECTOR the index found the item by.
//!
//! RULE under test: **the vector attribute an index finds an item by is retrieved only
//! where the request NAMES it — "a string that identifies one or more attributes to
//! retrieve from the index … if not specified, the operation returns all attributes
//! projected into the vector index" (`SearchVectors$ProjectionExpression`) — so a default
//! retrieval answers the entry's other attributes and a `ProjectionExpression` naming the
//! vector hands it back in the form the INDEX holds it in: "each element is a 32-bit
//! IEEE-754 floating point number" (`SearchVectors$SearchVector`, the shape
//! `VectorIndex$VectorAttribute` names on an item), so a coordinate no such float
//! represents exactly comes back as the one it rounds to. The TABLE's own item is
//! untouched: a `GetItem` of it answers the decimals the write carried.**
//!
//! The rows differ along what the rule generalises over: the index's `DistanceFunction`
//! (each of the three the model declares), its `Dimensions` (one, two and three), the name
//! of the vector attribute, the `ProjectionType` of the index (each of the three), and the
//! coordinates themselves — a whole number, a negative, a half, a value with more decimal
//! digits than a 32-bit float carries, and the first whole number that type cannot
//! represent (2^24 + 1, which it holds as 2^24).
//!
//! Nothing here waits a fixed span: each table is polled until the table and its index
//! report `ACTIVE` through the crate's shared bounded waiter, whose deadline is a backstop
//! against a transition that never happens rather than a measurement.
//!
//! Every operation is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};

use crate::emulator::{emulator, ok, search_result_attributes, table_when, REGION};

/// One row: a table carrying one vector index, the item written into it, and the coordinates
/// the index is expected to hold for that item.
struct Row {
    case: &'static str,
    table: &'static str,
    index: &'static str,
    /// The name of the vector attribute, which the index is keyed by.
    attribute: &'static str,
    distance: &'static str,
    projection: fn() -> Value,
    /// The coordinates the item is WRITTEN with, as the decimals a client sends.
    written: &'static [&'static str],
    /// The coordinates the index HOLDS, as the decimals a result carries them in.
    held: &'static [&'static str],
    /// The attribute names a result of a DEFAULT retrieval is expected to carry, sorted —
    /// the entry's attributes without the vector the index finds the item by.
    reported: &'static [&'static str],
}

/// The rows. Each index is declared over its own table, so one row's `Projection` and
/// `DistanceFunction` cannot be read off another's.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a COSINE index projecting everything",
            table: "vector-reported-cosine",
            index: "cosine-all",
            attribute: "emb",
            distance: "COSINE",
            projection: || json!({"ProjectionType": "ALL"}),
            written: &["1", "0"],
            held: &["1.0", "0.0"],
            reported: &["note", "pk"],
        },
        Row {
            case: "a EUCLIDEAN index projecting its keys alone",
            table: "vector-reported-euclidean",
            index: "euclidean-keys",
            attribute: "features",
            distance: "EUCLIDEAN",
            projection: || json!({"ProjectionType": "KEYS_ONLY"}),
            written: &["0.5", "-1.25", "2"],
            held: &["0.5", "-1.25", "2.0"],
            reported: &["pk"],
        },
        Row {
            case: "a DOT_PRODUCT index over a coordinate no 32-bit float holds",
            table: "vector-reported-dot-product",
            index: "dot-product-include",
            attribute: "v",
            distance: "DOT_PRODUCT",
            projection: || json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note"]}),
            written: &["16777217"],
            held: &["16777216.0"],
            reported: &["note", "pk"],
        },
        Row {
            case: "a coordinate with more decimal digits than the type carries",
            table: "vector-reported-precision",
            index: "precision-all",
            attribute: "emb",
            distance: "COSINE",
            projection: || json!({"ProjectionType": "ALL"}),
            written: &["1.2345678901234567", "16777218"],
            held: &["1.2345679", "16777218.0"],
            reported: &["note", "pk"],
        },
    ]
}

impl Row {
    /// The vector the item is written with, in DynamoDB list form.
    fn written_vector(&self) -> Value {
        Value::Array(
            self.written
                .iter()
                .map(|component| json!({"N": component}))
                .collect(),
        )
    }

    /// The vector a search of this row's index asks for: the coordinates the item carries,
    /// so the item is the nearest one whatever the distance function.
    fn search_vector(&self) -> Value {
        self.written_vector()
    }

    /// The coordinates a result is expected to carry, in DynamoDB list form.
    fn held_vector(&self) -> Value {
        Value::Array(
            self.held
                .iter()
                .map(|component| json!({"N": component}))
                .collect(),
        )
    }

    /// The item this row writes: the key, an attribute no `KEYS_ONLY` index projects, and
    /// the vector under this row's own attribute name.
    fn item(&self) -> Value {
        let mut item = ::serde_json::Map::new();
        item.insert("pk".to_string(), json!({"S": "item-1"}));
        item.insert("note".to_string(), json!({"S": "kept"}));
        item.insert(
            self.attribute.to_string(),
            json!({"L": self.written_vector()}),
        );
        Value::Object(item)
    }
}

/// Create the table one row searches, with its one vector index over that row's attribute.
async fn create(router: &smithy_server_runtime::MultiServiceRouter, row: &Row) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": row.table,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [{
                "IndexName": row.index,
                "VectorAttribute": {"AttributeName": row.attribute},
                "Dimensions": row.written.len(),
                "DistanceFunction": row.distance,
                "Projection": (row.projection)(),
            }],
        }),
    )
    .await;
}

/// Poll `DescribeTable` until the table of `row` and its vector index both report `ACTIVE` —
/// the waiter a client runs before searching one, since "the index must be in the ACTIVE
/// state" (`SearchVectors$IndexName`). The crate's shared bounded waiter does the polling
/// ([`crate::emulator::table_when`]), so this suite carries no deadline of its own.
async fn ready(router: &smithy_server_runtime::MultiServiceRouter, row: &Row) {
    table_when(router, row.case, REGION, row.table, |table| {
        table["TableStatus"] == json!("ACTIVE")
            && table["VectorIndexes"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|declared| {
                    declared["IndexName"] == json!(row.index)
                        && declared["IndexStatus"] == json!("ACTIVE")
                })
    })
    .await;
}

/// The attribute names the first result of `answered` carries, sorted.
fn reported_attributes(answered: &Value) -> Vec<String> {
    search_result_attributes(answered, 0)
}

#[tokio::test]
async fn a_default_retrieval_leaves_out_the_vector_the_index_finds_items_by() {
    let router = emulator();
    let rows = rows();
    // Every table is created first, so the spans the clock owes their transitions run
    // together rather than one after another.
    for row in &rows {
        create(&router, row).await;
    }

    for row in &rows {
        ready(&router, row).await;
        ok(
            &router,
            "PutItem",
            json!({"TableName": row.table, "Item": row.item()}),
        )
        .await;

        let answered = ok(
            &router,
            "SearchVectors",
            json!({
                "TableName": row.table,
                "IndexName": row.index,
                "SearchVector": row.search_vector(),
                "TopK": 1,
            }),
        )
        .await;

        let reported = reported_attributes(&answered);
        assert_eq!(
            reported,
            row.reported.to_vec(),
            "case {:?}: expected a default retrieval to carry {:?}, got {reported:?} from \
             {answered}",
            row.case,
            row.reported
        );
        let carried = answered["SearchResults"][0]["Item"].get(row.attribute);
        assert_eq!(
            carried, None,
            "case {:?}: a request naming no projection retrieves no vector, got {answered}",
            row.case
        );

        // The item the TABLE holds keeps the decimals the write carried: what a search hands
        // back is the index's own copy.
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": row.table, "Key": {"pk": {"S": "item-1"}}}),
        )
        .await;
        assert_eq!(
            read["Item"][row.attribute]["L"],
            row.written_vector(),
            "case {:?}: the stored item keeps the decimals it was written with ({:?}), got \
             {read}",
            row.case,
            row.written
        );
    }
}

#[tokio::test]
async fn a_projection_may_narrow_a_result_to_the_indexed_vector_alone() {
    let router = emulator();
    let rows = rows();
    for row in &rows {
        create(&router, row).await;
    }

    for row in &rows {
        ready(&router, row).await;
        ok(
            &router,
            "PutItem",
            json!({"TableName": row.table, "Item": row.item()}),
        )
        .await;

        // "Only attributes projected into the vector index can be retrieved"
        // (`SearchVectors$ProjectionExpression`): a request may narrow what the index
        // projects, and the vector attribute is one of the names it may narrow to.
        let answered = ok(
            &router,
            "SearchVectors",
            json!({
                "TableName": row.table,
                "IndexName": row.index,
                "SearchVector": row.search_vector(),
                "TopK": 1,
                "ProjectionExpression": row.attribute,
            }),
        )
        .await;

        let reported = reported_attributes(&answered);
        assert_eq!(
            reported,
            vec![row.attribute.to_string()],
            "case {:?}: a projection naming the vector reports that attribute alone, got \
             {reported:?} from {answered}",
            row.case
        );
        let carried = answered["SearchResults"][0]["Item"][row.attribute]["L"].clone();
        assert_eq!(
            carried,
            row.held_vector(),
            "case {:?}: the narrowed result carries the coordinates the index holds ({:?}), \
             got {carried} from {answered}",
            row.case,
            row.held
        );
    }
}
