// SPDX-License-Identifier: CC0-1.0

//! What a READ of an item carrying a vector is charged for.
//!
//! The rule pinned here: a vector attribute is a list of 32-bit IEEE-754 floating point
//! numbers (`SearchVectors$SearchVector`), so the store holds it at four bytes a dimension
//! and every capacity figure over such an item is sized that way — a read's as much as a
//! write's. What a read costs therefore follows the index's dimension COUNT and never the
//! number of digits each coordinate was written with: the same vector spelled with one
//! digit a coordinate and with eighteen is the same bytes, and both are charged the
//! per-dimension cost rather than the decimal text.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, and the rule is asserted over a
//! SET of inputs — every read surface that reports `ConsumedCapacity`, each at both
//! consistencies where it offers the choice, over coordinates of two very different
//! widths.

use serde_json::{json, Value};

use crate::emulator::{describe_when_active, emulator, ok};

/// The table every row reads.
const TABLE: &str = "VectorReadCapacity";

/// The dimensions of the vector the rows read: wide enough that the stored form crosses a
/// read unit's worth of bytes, so the figure is not the one-unit minimum.
const DIMENSIONS: usize = 1_024;

/// What one dimension of a stored vector costs ([`SearchVectors$SearchVector`]: a 32-bit
/// float).
const BYTES_PER_DIMENSION: i64 = 4;

/// The bytes one READ capacity unit covers: "one read request unit represents one strongly
/// consistent read request ... for an item up to 4 KB in size" (the DynamoDB Developer
/// Guide, "Read/write capacity mode").
const READ_UNIT_BYTES: i64 = 4 * 1024;

/// The metered size of the item one row writes: its key, and its vector attribute at four
/// bytes a dimension — the form the store holds it in, whatever text it arrived as.
fn metered_size(key: &str) -> i64 {
    let key_bytes = 2 + i64::try_from(key.len()).expect("the key fits");
    let vector_bytes = 3 + i64::try_from(DIMENSIONS).expect("the dimension count fits")
        * BYTES_PER_DIMENSION;
    key_bytes + vector_bytes
}

/// The units a strongly consistent read of that item consumes: one per read unit of its
/// metered size, never fewer than one.
fn consistent_units(key: &str) -> f64 {
    let size = metered_size(key);
    let units = (size / READ_UNIT_BYTES + i64::from(size % READ_UNIT_BYTES != 0)).max(1);
    units as f64
}

/// One vector of [`DIMENSIONS`] coordinates, each spelled with `digits` significant digits,
/// so the TEXT of the attribute grows with `digits` while the vector it denotes does not.
fn vector(digits: usize) -> Value {
    let coordinate = format!("0.{}", "1".repeat(digits));
    json!({"L": (0..DIMENSIONS)
        .map(|_| json!({"N": coordinate.clone()}))
        .collect::<Vec<Value>>()})
}

/// Create the table and write one item per coordinate width.
async fn table(router: &smithy_server_runtime::MultiServiceRouter, keys: &[(&str, usize)]) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [{
                "IndexName": "vec-idx",
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": DIMENSIONS,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(router, TABLE, TABLE).await;
    for (key, digits) in keys {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": key}, "emb": vector(*digits)},
            }),
        )
        .await;
    }
}

/// A number the response carried, or `None` where the member is absent.
fn figure(consumed: &Value, path: &[&str]) -> Option<f64> {
    let mut value = consumed;
    for member in path {
        value = value.get(*member)?;
    }
    value.as_f64()
}

#[tokio::test]
async fn a_read_of_an_item_carrying_a_vector_is_sized_by_its_dimension_count() {
    let router = emulator();
    // Two items holding the SAME vector, one spelled with a single digit a coordinate and
    // one with eighteen: their stored text differs by kilobytes and their metered size does
    // not, so every row below must charge them alike.
    let keys = [("narrow", 1usize), ("wide", 18)];
    table(&router, &keys).await;

    let expected = consistent_units("narrow");
    assert_eq!(
        expected,
        consistent_units("wide"),
        "the two rows differ only in the text their coordinates were written with"
    );
    assert!(
        expected > 1.0,
        "the vector must cross a read unit for the figure to be more than the minimum, got \
         {expected}"
    );

    for (key, digits) in keys {
        // Rows differ in the surface that reads the item and in the consistency it reads
        // at: a strongly consistent read of the item costs one unit per read unit of its
        // metered size, an eventually consistent one half of that, and a transactional read
        // twice ("Transactional read requests require two read capacity units").
        let cases: [(&str, &str, Value, f64); 7] = [
            (
                "a consistent GetItem",
                "GetItem",
                json!({"TableName": TABLE, "Key": {"pk": {"S": key}},
                       "ConsistentRead": true, "ReturnConsumedCapacity": "TOTAL"}),
                expected,
            ),
            (
                "an eventually consistent GetItem",
                "GetItem",
                json!({"TableName": TABLE, "Key": {"pk": {"S": key}},
                       "ReturnConsumedCapacity": "TOTAL"}),
                expected / 2.0,
            ),
            (
                "a consistent Query",
                "Query",
                json!({"TableName": TABLE, "ConsistentRead": true,
                       "KeyConditionExpression": "pk = :k",
                       "ExpressionAttributeValues": {":k": {"S": key}},
                       "ReturnConsumedCapacity": "TOTAL"}),
                expected,
            ),
            (
                "an eventually consistent Query",
                "Query",
                json!({"TableName": TABLE,
                       "KeyConditionExpression": "pk = :k",
                       "ExpressionAttributeValues": {":k": {"S": key}},
                       "ReturnConsumedCapacity": "TOTAL"}),
                expected / 2.0,
            ),
            (
                "a consistent BatchGetItem",
                "BatchGetItem",
                json!({"RequestItems": {TABLE: {
                           "Keys": [{"pk": {"S": key}}], "ConsistentRead": true}},
                       "ReturnConsumedCapacity": "TOTAL"}),
                expected,
            ),
            (
                "a TransactGetItems",
                "TransactGetItems",
                json!({"TransactItems": [{"Get": {
                           "TableName": TABLE, "Key": {"pk": {"S": key}}}}],
                       "ReturnConsumedCapacity": "TOTAL"}),
                expected * 2.0,
            ),
            (
                "an ExecuteStatement SELECT of the item",
                "ExecuteStatement",
                json!({"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = '{key}'"),
                       "ConsistentRead": true,
                       "ReturnConsumedCapacity": "TOTAL"}),
                expected,
            ),
        ];

        for (case, operation, request, charged) in cases {
            let answered = ok(&router, operation, request).await;
            // The batched and transactional reads report a list of per-table figures; the
            // single-item ones report the figure itself.
            let consumed = if answered["ConsumedCapacity"].is_array() {
                answered["ConsumedCapacity"][0].clone()
            } else {
                answered["ConsumedCapacity"].clone()
            };
            let reported = figure(&consumed, &["CapacityUnits"]);
            assert_eq!(
                reported,
                Some(charged),
                "case {case:?} over the {digits}-digit row: expected {charged} unit(s) for the \
                 stored vector, got {reported:?} in {consumed}"
            );
        }
    }
}

#[tokio::test]
async fn a_scan_of_items_carrying_vectors_is_sized_the_same_way() {
    let router = emulator();
    // The same two rows, read in ONE page: a scan is charged per item it evaluated, so the
    // page's figure is the sum of the per-item charges — and the two items are charged
    // alike however their coordinates were spelled.
    let keys = [("narrow", 1usize), ("wide", 18)];
    table(&router, &keys).await;

    for (case, consistent, factor) in [
        ("a consistent Scan of both items", true, 1.0),
        ("an eventually consistent Scan of both items", false, 0.5),
    ] {
        let mut request = json!({"TableName": TABLE, "ReturnConsumedCapacity": "TOTAL"});
        if consistent {
            request["ConsistentRead"] = json!(true);
        }
        let answered = ok(&router, "Scan", request).await;
        let expected = (consistent_units("narrow") + consistent_units("wide")) * factor;
        let reported = figure(&answered["ConsumedCapacity"], &["CapacityUnits"]);
        assert_eq!(
            reported,
            Some(expected),
            "case {case:?}: expected {expected} unit(s) for the two stored vectors, got \
             {reported:?} in {answered}"
        );
        assert_eq!(
            answered["ScannedCount"], json!(2),
            "case {case:?}: both items were evaluated, got {answered}"
        );
    }
}
