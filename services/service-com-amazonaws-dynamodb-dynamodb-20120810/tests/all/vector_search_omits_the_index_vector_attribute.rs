// SPDX-License-Identifier: CC0-1.0

//! Which of an item's attributes a `SearchVectors` result hands back.
//!
//! RULE under test: **a result carries "the projected attributes of the item returned by the
//! vector search" (`SearchResultItem$Item`), and the vector the SEARCHED index is built on
//! is not one of them — it is what the index found the item by, and the score is what a
//! result reports of it — so the attribute that index's `VectorAttribute` names is absent
//! from every result unless the request's `ProjectionExpression` asks for it by name, which
//! "identifies one or more attributes to retrieve from the index"
//! (`SearchVectors$ProjectionExpression`). Every other attribute the index projects is
//! handed back as before, and the TABLE's item keeps the vector either way.**
//!
//! The rows differ along what the rule generalises over: the `ProjectionType` of the index
//! (each of the three the model declares, plus an `INCLUDE` that names the searched index's
//! OWN vector attribute and one that names another index's), which vector attribute of the
//! same item the index is built on (so one index's vector is another's ordinary projected
//! attribute — the omission follows the index SEARCHED, not the shape of the value), the
//! `DistanceFunction`, the form of the table's key (a partition key alone and a composite
//! key), whether the index carries a search schema, and the `ProjectionExpression` the
//! request narrows with (absent, the vector alone, the vector beside a key, and a name the
//! index holds nothing for).
//!
//! Nothing here waits a fixed span: each table is polled until its indexes report `ACTIVE`
//! through the crate's shared bounded waiter, whose deadline is a backstop against a
//! transition that never happens rather than a measurement.
//!
//! Every operation is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};

use crate::emulator::{emulator, ok, search_result_attributes, table_when, REGION};

/// The two-dimensional vector both vector attributes of the item below carry, and the vector
/// every search asks for, so the item is the nearest one whatever the distance function.
fn embedding() -> Value {
    json!([{"N": "1"}, {"N": "0"}])
}

/// The coordinates the index HOLDS for that vector: 32-bit floats
/// (`SearchVectors$SearchVector`), which is the form a result carries when a projection asks
/// for the vector attribute by name.
fn held_embedding() -> Value {
    json!([{"N": "1.0"}, {"N": "0.0"}])
}

/// One vector index declaration over `attribute`.
fn index(name: &str, attribute: &str, distance: &str, projection: Value) -> Value {
    json!({
        "IndexName": name,
        "VectorAttribute": {"AttributeName": attribute},
        "Dimensions": 2,
        "DistanceFunction": distance,
        "Projection": projection,
    })
}

/// Poll `DescribeTable` until every vector index of `table` reports `ACTIVE` — the waiter a
/// client runs before searching one, since "the index must be in the ACTIVE state"
/// (`SearchVectors$IndexName`). The crate's shared bounded waiter does the polling, so this
/// suite carries no deadline of its own.
async fn indexes_ready(router: &smithy_server_runtime::MultiServiceRouter, case: &str, table: &str) {
    table_when(router, case, REGION, table, |described| {
        let declared = described["VectorIndexes"].as_array().cloned().unwrap_or_default();
        !declared.is_empty()
            && declared
                .iter()
                .all(|index| index["IndexStatus"] == json!("ACTIVE"))
    })
    .await;
}

/// One `SearchVectors` through `index` of `table`, narrowed by `projection_expression` when
/// the row names one and pinning `tenant` when the index partitions by it.
async fn search(
    router: &smithy_server_runtime::MultiServiceRouter,
    table: &str,
    index: &str,
    partitioned: bool,
    projection_expression: Option<&str>,
) -> Value {
    let mut body = json!({
        "TableName": table,
        "IndexName": index,
        "SearchVector": embedding(),
        "TopK": 5,
    });
    if partitioned {
        body["SearchConditionExpression"] = json!("tenant = :t");
        body["ExpressionAttributeValues"] = json!({":t": {"S": "t1"}});
    }
    if let Some(expression) = projection_expression {
        body["ProjectionExpression"] = json!(expression);
    }
    ok(router, "SearchVectors", body).await
}

/// One row: the index a search names, the vector attribute it is built on, and the attributes
/// a result through it carries — with no `ProjectionExpression`, and narrowed to that vector
/// attribute beside the table's partition key.
struct Row {
    index: &'static str,
    /// The attribute the index finds an item by, which a result must not carry on its own.
    vector: &'static str,
    /// The attribute names a result carries when the request narrows nothing, sorted.
    whole: &'static [&'static str],
    /// The attribute names a result carries when the request names "<vector>, pk", sorted.
    narrowed: &'static [&'static str],
}

/// The rows, over one table holding one item with TWO vector attributes: `alpha` and `beta`.
/// An index built on `alpha` must omit `alpha` and may still hand `beta` back, and the index
/// built on `beta` is the same statement the other way round.
fn rows() -> &'static [Row] {
    &[
        // "ALL - All of the table attributes are projected into the index"
        // (`Projection$ProjectionType`), so everything but the vector searched comes back.
        Row {
            index: "alpha-all",
            vector: "alpha",
            whole: &["beta", "extra", "note", "pk"],
            narrowed: &["alpha", "pk"],
        },
        // "KEYS_ONLY - Only the index and primary key attributes are projected": the index
        // holds the table's key and the vector it finds items by, so a result is the key
        // alone. This is the shape of the recorded case.
        Row {
            index: "alpha-keys-only",
            vector: "alpha",
            whole: &["pk"],
            narrowed: &["alpha", "pk"],
        },
        // An `INCLUDE` naming the OTHER index's vector attribute: that one is an ordinary
        // projected attribute here, so it is handed back while the searched index's own is
        // not.
        Row {
            index: "alpha-include-beta",
            vector: "alpha",
            whole: &["beta", "note", "pk"],
            narrowed: &["alpha", "pk"],
        },
        // An `INCLUDE` naming the searched index's OWN vector attribute: naming it in the
        // index's projection does not make it one of the attributes a result reports.
        Row {
            index: "alpha-include-alpha",
            vector: "alpha",
            whole: &["pk"],
            narrowed: &["alpha", "pk"],
        },
        // The same rule asked of the index built on `beta`: now `alpha` is the ordinary
        // attribute and `beta` the one omitted.
        Row {
            index: "beta-all",
            vector: "beta",
            whole: &["alpha", "extra", "note", "pk"],
            narrowed: &["beta", "pk"],
        },
    ]
}

/// The table every row above searches, created with its five vector indexes and polled until
/// each reports `ACTIVE`, holding one item under `pk` `item-1`.
async fn table(router: &smithy_server_runtime::MultiServiceRouter) -> &'static str {
    let table = "vector-search-omits-the-index-vector";
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": table,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [
                index("alpha-all", "alpha", "COSINE", json!({"ProjectionType": "ALL"})),
                index(
                    "alpha-keys-only",
                    "alpha",
                    "EUCLIDEAN",
                    json!({"ProjectionType": "KEYS_ONLY"}),
                ),
                index(
                    "alpha-include-beta",
                    "alpha",
                    "DOT_PRODUCT",
                    json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["beta", "note"]}),
                ),
                index(
                    "alpha-include-alpha",
                    "alpha",
                    "COSINE",
                    json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["alpha"]}),
                ),
                index("beta-all", "beta", "COSINE", json!({"ProjectionType": "ALL"})),
            ],
        }),
    )
    .await;
    indexes_ready(router, "the table carrying the vector indexes", table).await;
    ok(
        router,
        "PutItem",
        json!({"TableName": table, "Item": {
            "pk": {"S": "item-1"},
            "note": {"S": "kept"},
            "extra": {"S": "unprojected"},
            "alpha": {"L": embedding()},
            "beta": {"L": embedding()},
        }}),
    )
    .await;
    table
}

#[tokio::test]
async fn a_search_result_omits_the_vector_attribute_of_the_index_it_searched() {
    let router = emulator();
    let table = table(&router).await;

    for row in rows() {
        let answered = search(&router, table, row.index, false, None).await;
        let reported = search_result_attributes(&answered, 0);
        assert_eq!(
            reported,
            row.whole.to_vec(),
            "case {:?}: expected a result to carry {:?} — not the {:?} the index finds items \
             by — got {reported:?} from {answered}",
            row.index,
            row.whole,
            row.vector
        );
        assert!(
            answered["SearchResults"][0]["Item"][row.vector].is_null(),
            "case {:?}: the vector attribute {:?} is not one of the attributes a result \
             projects back, got {answered}",
            row.index,
            row.vector
        );
    }

    // The TABLE's item is untouched by what a search of its indexes hands back: both vectors
    // are still stored, and still read back through `GetItem`.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": table, "Key": {"pk": {"S": "item-1"}}}),
    )
    .await;
    for attribute in ["alpha", "beta"] {
        assert_eq!(
            read["Item"][attribute],
            json!({"L": embedding()}),
            "case {attribute:?}: the stored item keeps the vector a search leaves out of its \
             results, got {read}"
        );
    }
}

#[tokio::test]
async fn a_projection_expression_naming_the_vector_attribute_retrieves_it() {
    let router = emulator();
    let table = table(&router).await;

    for row in rows() {
        // "A string that identifies one or more attributes to retrieve from the index"
        // (`SearchVectors$ProjectionExpression`): asked for by name, the vector comes back —
        // in the form the INDEX holds it, a list of 32-bit floats.
        let expression = format!("{}, pk", row.vector);
        let answered = search(&router, table, row.index, false, Some(&expression)).await;
        let reported = search_result_attributes(&answered, 0);
        assert_eq!(
            reported,
            row.narrowed.to_vec(),
            "case {:?} narrowed to {expression:?}: expected {:?}, got {reported:?} from \
             {answered}",
            row.index,
            row.narrowed
        );
        let carried = answered["SearchResults"][0]["Item"][row.vector]["L"].clone();
        assert_eq!(
            carried,
            held_embedding(),
            "case {:?} narrowed to {expression:?}: the result carries the coordinates the \
             index holds, got {carried} from {answered}",
            row.index
        );

        // A name the index holds nothing for reports nothing for it, which a projection
        // naming only the vector of ANOTHER attribute's index shows on a `KEYS_ONLY` index.
        let narrowed = search(&router, table, row.index, false, Some("note")).await;
        let reported = search_result_attributes(&narrowed, 0);
        let expected: Vec<String> = row
            .whole
            .iter()
            .filter(|name| **name == "note")
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            reported, expected,
            "case {:?} narrowed to \"note\": a projection reports only what the index holds, \
             expected {expected:?}, got {reported:?} from {narrowed}",
            row.index
        );
    }
}

#[tokio::test]
async fn the_rule_holds_over_a_composite_key_and_a_search_schema() {
    let router = emulator();
    let table = "vector-search-omits-the-index-vector-composite";
    // A table keyed by a composite key, whose index carries a search schema: the key
    // attributes and the schema's attributes are projected into the index whatever its
    // `Projection` says, and the vector it is built on is still not one of the attributes a
    // result reports.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": table,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "tenant", "AttributeType": "S"},
                {"AttributeName": "tier", "AttributeType": "S"},
            ],
            "VectorIndexes": [{
                "IndexName": "filtered-keys-only",
                "VectorAttribute": {"AttributeName": "vec"},
                "Dimensions": 2,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "KEYS_ONLY"},
                "SearchSchema": [
                    {"AttributeName": "tenant", "SearchSchemaElementType": "HASH"},
                    {"AttributeName": "tier", "SearchSchemaElementType": "INLINE_FILTER"},
                ],
            }],
        }),
    )
    .await;
    indexes_ready(&router, "the table keyed by a composite key", table).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": table, "Item": {
            "pk": {"S": "item-1"},
            "sk": {"N": "7"},
            "tenant": {"S": "t1"},
            "tier": {"S": "gold"},
            "note": {"S": "unprojected"},
            "vec": {"L": embedding()},
        }}),
    )
    .await;

    let answered = search(&router, table, "filtered-keys-only", true, None).await;
    let reported = search_result_attributes(&answered, 0);
    assert_eq!(
        reported,
        vec![
            "pk".to_string(),
            "sk".to_string(),
            "tenant".to_string(),
            "tier".to_string(),
        ],
        "case \"a composite key and a search schema\": both key attributes and both schema \
         attributes are projected back, and the vector the index is built on is not, got \
         {reported:?} from {answered}"
    );

    let narrowed = search(&router, table, "filtered-keys-only", true, Some("vec")).await;
    let reported = search_result_attributes(&narrowed, 0);
    assert_eq!(
        reported,
        vec!["vec".to_string()],
        "case \"a composite key and a search schema\" narrowed to \"vec\": the vector is \
         retrievable by name, got {reported:?} from {narrowed}"
    );
    let carried = narrowed["SearchResults"][0]["Item"]["vec"]["L"].clone();
    assert_eq!(
        carried,
        held_embedding(),
        "case \"a composite key and a search schema\": the narrowed result carries the \
         coordinates the index holds, got {carried} from {narrowed}"
    );
}
