// SPDX-License-Identifier: CC0-1.0

//! Whether a `SearchVectors` result carries the VECTOR the index found the item by, and what
//! leaving it out changes about the search.
//!
//! RULE under test: a result "contains the projected attributes and a similarity score"
//! (`SearchVectors$SearchResults`), and "only attributes projected into the vector index can
//! be retrieved" (`SearchVectors$ProjectionExpression`) — what is projected into an index is
//! what its `Projection` copies there (`VectorIndex$Projection`) beside the primary key and
//! index key attributes, "which are automatically projected" (`Projection`). The vector
//! attribute is the one projected attribute a DEFAULT retrieval leaves out: it is what the
//! index FINDS an item by, and "if not specified, the operation returns all attributes
//! projected into the vector index" answers the entry's other attributes, so it comes back
//! only where the request NAMES it. Either way the index still holds the vector, so every
//! index over the same items finds the same items in the same order with the same scores.
//!
//! The table spans the rule along the index's `Projection` — each `ProjectionType` the model
//! declares, plus an `INCLUDE` that names the vector attribute itself — and each row is read
//! twice: with no `ProjectionExpression` (the projected attributes but the vector) and with
//! one naming the vector alone (which retrieves it through every one of them).
//!
//! Every operation is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use, and nothing waits a fixed span: the table and its indexes
//! are polled until they report `ACTIVE` through the crate's shared bounded waiter.

use serde_json::{json, Value};

use crate::emulator::{emulator, ok, search_result_attributes, table_when, REGION};

/// The table every row searches, carrying one vector index per row.
const TABLE: &str = "vector-withheld-projection";

/// The vector attribute every index of the table is over.
const VECTOR: &str = "emb";

/// One row: a vector index over [`VECTOR`] and the attributes a search through it reports.
struct Row {
    index: &'static str,
    projection: Value,
    /// The attribute names a result of an unnarrowed search carries, sorted.
    reported: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            index: "all",
            projection: json!({"ProjectionType": "ALL"}),
            reported: &["note", "pk", "unprojected"],
        },
        Row {
            index: "keys-only",
            projection: json!({"ProjectionType": "KEYS_ONLY"}),
            reported: &["pk"],
        },
        Row {
            index: "include-note",
            projection: json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note"]}),
            reported: &["note", "pk"],
        },
        // Naming the vector attribute among the included ones changes nothing about a
        // DEFAULT retrieval: the vector is left out of it whatever the projection stores.
        Row {
            index: "include-the-vector",
            projection: json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note", "emb"]}),
            reported: &["note", "pk"],
        },
    ]
}

/// The two items the table holds, each with its own two-dimensional vector, so a search has
/// a nearer and a farther item to order.
fn items() -> Vec<Value> {
    vec![
        json!({
            "pk": {"S": "near"},
            "note": {"S": "kept"},
            "unprojected": {"S": "outside every INCLUDE"},
            "emb": {"L": [{"N": "1.0"}, {"N": "0.0"}]},
        }),
        json!({
            "pk": {"S": "far"},
            "note": {"S": "kept"},
            "unprojected": {"S": "outside every INCLUDE"},
            "emb": {"L": [{"N": "0.0"}, {"N": "1.0"}]},
        }),
    ]
}

/// One `SearchVectors` through `index`, optionally narrowed by `projection_expression`.
async fn search(
    router: &smithy_server_runtime::MultiServiceRouter,
    index: &str,
    projection_expression: Option<&str>,
) -> Value {
    let mut body = json!({
        "TableName": TABLE,
        "IndexName": index,
        "SearchVector": [{"N": "1.0"}, {"N": "0.0"}],
        "TopK": 2,
    });
    if let Some(expression) = projection_expression {
        body["ProjectionExpression"] = json!(expression);
    }
    ok(router, "SearchVectors", body).await
}

/// The `pk` of each result, in the order the search reported them.
fn keys(answered: &Value) -> Vec<String> {
    answered["SearchResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|result| {
            result["Item"]["pk"]["S"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The score of each result, in the order the search reported them.
fn scores(answered: &Value) -> Vec<f64> {
    answered["SearchResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|result| result["Score"].as_f64().unwrap_or(f64::NAN))
        .collect()
}

#[tokio::test]
async fn a_default_retrieval_leaves_out_the_vector_it_found_its_items_by() {
    let router = emulator();
    let rows = rows();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": rows
                .iter()
                .map(|row| json!({
                    "IndexName": row.index,
                    "VectorAttribute": {"AttributeName": VECTOR},
                    "Dimensions": 2,
                    "DistanceFunction": "COSINE",
                    "Projection": row.projection.clone(),
                }))
                .collect::<Vec<Value>>(),
        }),
    )
    .await;
    let declared_count = rows.len();
    // The indexes settle on the clock, so the table is polled until every one of them
    // reports `ACTIVE` — "the index must be in the ACTIVE state" (`SearchVectors$IndexName`).
    table_when(&router, "the table carrying every projection", REGION, TABLE, |table| {
        let declared = table["VectorIndexes"].as_array().cloned().unwrap_or_default();
        declared.len() == declared_count
            && declared
                .iter()
                .all(|index| index["IndexStatus"] == json!("ACTIVE"))
    })
    .await;
    for item in items() {
        ok(&router, "PutItem", json!({"TableName": TABLE, "Item": item})).await;
    }

    let mut every_key_order: Vec<Vec<String>> = Vec::new();
    let mut every_score: Vec<Vec<f64>> = Vec::new();
    for row in &rows {
        let answered = search(&router, row.index, None).await;
        let reported = search_result_attributes(&answered, 0);
        assert_eq!(
            reported,
            row.reported.to_vec(),
            "case {:?}: expected a result to carry {:?}, got {reported:?} from {answered}",
            row.index,
            row.reported
        );

        // A `ProjectionExpression` naming the vector is what it takes to retrieve it, and
        // it is retrievable through every index: each one holds the vector it finds its
        // items by.
        let narrowed = search(&router, row.index, Some(VECTOR)).await;
        let narrowed_names = search_result_attributes(&narrowed, 0);
        assert_eq!(
            narrowed_names,
            vec![VECTOR.to_string()],
            "case {:?} narrowed to {VECTOR:?}: expected that attribute alone, got \
             {narrowed_names:?} from {narrowed}",
            row.index
        );

        every_key_order.push(keys(&answered));
        every_score.push(scores(&answered));
    }

    // What a DEFAULT retrieval leaves out of the vector changes nothing about what the
    // search FINDS: the vector is held by every one of these indexes, so each reports the
    // same items, nearest first, with the same scores.
    let expected_keys = vec!["near".to_string(), "far".to_string()];
    for (row, order) in rows.iter().zip(&every_key_order) {
        assert_eq!(
            *order, expected_keys,
            "case {:?}: every index finds the same items nearest first, got {order:?}",
            row.index
        );
    }
    let first = every_score.first().cloned().unwrap_or_default();
    for (row, scored) in rows.iter().zip(&every_score) {
        assert_eq!(
            *scored, first,
            "case {:?}: every index scores the items by the vector it holds, so the scores \
             match the ones the index projecting everything reported ({first:?}), got \
             {scored:?}",
            row.index
        );
    }
}
