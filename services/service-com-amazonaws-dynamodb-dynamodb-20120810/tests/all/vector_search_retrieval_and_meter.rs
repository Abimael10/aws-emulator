// SPDX-License-Identifier: CC0-1.0

//! WHAT a `SearchVectors` retrieves of each hit, and WHAT the search is metered at.
//!
//! The two rules pinned here:
//!
//! 1. **the vector attribute an index finds its items by is retrieved only where the
//!    request NAMES it.** "A string that identifies one or more attributes to retrieve from
//!    the index. Separate attribute names with commas. If not specified, the operation
//!    returns all attributes projected into the vector index. Only attributes projected into
//!    the vector index can be retrieved" (`SearchVectors$ProjectionExpression`): the vector
//!    attribute IS projected into the index — it is the index's own key attribute — so a
//!    `ProjectionExpression` naming it hands it back, as the INDEX holds it ("each element
//!    is a 32-bit IEEE-754 floating point number", `SearchVectors$SearchVector`), while a
//!    request naming no projection is answered the entry's OTHER attributes and no
//!    embedding;
//! 2. **`ConsumedCapacity` reports the work the search did, in bytes, floored at one
//!    kilobyte.** `VectorSearchRequestBytes` is "the number of vector search request bytes
//!    consumed by a SearchVectors operation" (`VectorCapacity$VectorSearchRequestBytes`), so
//!    the figure grows with the number of results the request asked for, with the index's
//!    `Dimensions` — the vector each result was scored by is four bytes a dimension — and
//!    with the attributes the request retrieves, the embedding included; a search whose work
//!    is under the floor costs the floor.
//!
//! The rows span what each rule generalises over. For rule 1: the index's `Projection`
//! (`ALL`, `KEYS_ONLY`, and an `INCLUDE` that names a non-key attribute), its
//! `DistanceFunction` (each of the three the model declares), its `Dimensions` (one, two
//! and three), the vector attribute's NAME, and the form the request names it in (alone,
//! beside the key, in either order, and through an `ExpressionAttributeNames` alias) beside
//! a projection that names it not at all. For rule 2: the `TopK` asked for, the index's
//! `Dimensions`, whether the projection retrieves the embedding, and each
//! `ReturnConsumedCapacity` value.
//!
//! Nothing here waits a fixed span: each table is polled until it and its indexes report
//! `ACTIVE` through the crate's shared bounded waiter, whose deadline is a backstop against
//! a transition that never happens rather than a measurement.
//!
//! Every operation is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok, search_result_attributes, table_when, REGION};

/// The one-kilobyte floor a vector request is metered at
/// (`state::MIN_VECTOR_REQUEST_BYTES`).
const FLOOR_BYTES: f64 = 1024.0;

/// Poll until `table` and every vector index it declares report `ACTIVE` — "the index must
/// be in the ACTIVE state" (`SearchVectors$IndexName`).
async fn ready(router: &MultiServiceRouter, case: &str, table: &str) {
    table_when(router, case, REGION, table, |described| {
        described["TableStatus"] == json!("ACTIVE")
            && described["VectorIndexes"]
                .as_array()
                .is_some_and(|indexes| {
                    !indexes.is_empty()
                        && indexes
                            .iter()
                            .all(|index| index["IndexStatus"] == json!("ACTIVE"))
                })
    })
    .await;
}

/// One vector index declaration.
fn index(name: &str, attribute: &str, dimensions: usize, distance: &str, projection: Value) -> Value {
    json!({
        "IndexName": name,
        "VectorAttribute": {"AttributeName": attribute},
        "Dimensions": dimensions,
        "DistanceFunction": distance,
        "Projection": projection,
    })
}

/// A vector of `dimensions` coordinates, the first one `1` and the rest `0`, as a client
/// sends it.
fn vector(dimensions: usize) -> Value {
    Value::Array(
        (0..dimensions)
            .map(|at| json!({"N": if at == 0 { "1" } else { "0" }}))
            .collect(),
    )
}

/// The same vector as the INDEX holds it: 32-bit floats, which this service renders with the
/// fraction they carry (`state::stored_vector`).
fn held(dimensions: usize) -> Value {
    Value::Array(
        (0..dimensions)
            .map(|at| json!({"N": if at == 0 { "1.0" } else { "0.0" }}))
            .collect(),
    )
}

/// One row of rule 1: an index of the retrieval table, and the attributes a DEFAULT
/// retrieval through it carries.
struct Retrieval {
    index: &'static str,
    /// The vector attribute this index finds items by, which is the one name a default
    /// retrieval leaves out.
    attribute: &'static str,
    dimensions: usize,
    /// The attributes a request naming no projection is answered, sorted.
    default: &'static [&'static str],
}

/// The table every rule-1 row searches: one key, one non-key attribute, and three vector
/// indexes differing in projection, distance function, dimensions and vector attribute name.
const RETRIEVAL_TABLE: &str = "vector-retrieval-rule";

/// The rows. An `ALL` index projects the OTHER indexes' vector attributes too, so its
/// default retrieval shows that what is withheld is the searched index's OWN vector
/// attribute and not every list of numbers the item carries.
const RETRIEVALS: &[Retrieval] = &[
    Retrieval {
        index: "all-idx",
        attribute: "emb",
        dimensions: 2,
        default: &["note", "pk", "trio", "single"],
    },
    Retrieval {
        index: "keys-idx",
        attribute: "trio",
        dimensions: 3,
        default: &["pk"],
    },
    Retrieval {
        index: "include-idx",
        attribute: "single",
        dimensions: 1,
        default: &["note", "pk"],
    },
];

/// Create the rule-1 table and write the one item every index holds an entry for.
async fn retrieval_table(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": RETRIEVAL_TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [
                index("all-idx", "emb", 2, "COSINE", json!({"ProjectionType": "ALL"})),
                index(
                    "keys-idx",
                    "trio",
                    3,
                    "EUCLIDEAN",
                    json!({"ProjectionType": "KEYS_ONLY"}),
                ),
                index(
                    "include-idx",
                    "single",
                    1,
                    "DOT_PRODUCT",
                    json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note"]}),
                ),
            ],
        }),
    )
    .await;
    ready(router, "the retrieval table", RETRIEVAL_TABLE).await;
    ok(
        router,
        "PutItem",
        json!({"TableName": RETRIEVAL_TABLE, "Item": {
            "pk": {"S": "i1"},
            "note": {"S": "kept"},
            "emb": {"L": vector(2)},
            "trio": {"L": vector(3)},
            "single": {"L": vector(1)},
        }}),
    )
    .await;
}

/// One `SearchVectors` through `row`'s index, with the `ProjectionExpression` and
/// `ExpressionAttributeNames` given (either absent as `Value::Null`).
async fn search(
    router: &MultiServiceRouter,
    row: &Retrieval,
    projection: Value,
    names: Value,
) -> Value {
    let mut body = json!({
        "TableName": RETRIEVAL_TABLE,
        "IndexName": row.index,
        "SearchVector": vector(row.dimensions),
        "TopK": 5,
    });
    if !projection.is_null() {
        body["ProjectionExpression"] = projection;
    }
    if !names.is_null() {
        body["ExpressionAttributeNames"] = names;
    }
    ok(router, "SearchVectors", body).await
}

/// The attribute names of the first result, sorted.
fn reported(answered: &Value) -> Vec<String> {
    search_result_attributes(answered, 0)
}

/// `expected` as the sorted `Vec<String>` the readings above answer.
fn sorted(expected: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = expected.iter().map(ToString::to_string).collect();
    names.sort();
    names
}

/// RULE 1, the default half: a request naming no projection retrieves the entry's other
/// attributes and no embedding, whatever the index's `Projection`, `DistanceFunction` or
/// `Dimensions`.
#[tokio::test]
async fn a_default_retrieval_withholds_the_searched_index_vector_attribute() {
    let router = emulator();
    retrieval_table(&router).await;

    for row in RETRIEVALS {
        let answered = search(&router, row, Value::Null, Value::Null).await;
        let names = reported(&answered);
        assert_eq!(
            names,
            sorted(row.default),
            "case {:?}: expected a default retrieval to carry {:?}, got {names:?} from \
             {answered}",
            row.index,
            sorted(row.default)
        );
        assert_eq!(
            answered["SearchResults"][0]["Item"].get(row.attribute),
            None,
            "case {:?}: the vector attribute {:?} is retrieved only where the request names \
             it, got {answered}",
            row.index,
            row.attribute
        );
    }
}

/// RULE 1, the named half: every form of `ProjectionExpression` that names the vector
/// attribute retrieves it — alone, beside the key in either order, and through an
/// `ExpressionAttributeNames` alias — and the value it carries is the one the INDEX holds.
#[tokio::test]
async fn a_projection_expression_naming_the_vector_attribute_retrieves_it() {
    let router = emulator();
    retrieval_table(&router).await;

    for row in RETRIEVALS {
        // The forms a request can name the attribute in, each with the whole attribute set
        // it is expected to answer.
        let forms: Vec<(String, Value, Value, Vec<String>)> = vec![
            (
                "the vector attribute alone".to_string(),
                json!(row.attribute),
                Value::Null,
                vec![row.attribute.to_string()],
            ),
            (
                "the vector attribute after the key".to_string(),
                json!(format!("pk, {}", row.attribute)),
                Value::Null,
                sorted(&["pk", row.attribute]),
            ),
            (
                "the vector attribute before the key".to_string(),
                json!(format!("{}, pk", row.attribute)),
                Value::Null,
                sorted(&["pk", row.attribute]),
            ),
            (
                "the vector attribute through an alias".to_string(),
                json!("#v"),
                json!({"#v": row.attribute}),
                vec![row.attribute.to_string()],
            ),
        ];
        for (form, projection, names, expected) in forms {
            let case = format!("{} naming {form}", row.index);
            let answered = search(&router, row, projection, names).await;
            let carried = reported(&answered);
            assert_eq!(
                carried, expected,
                "case {case:?}: expected the result to carry {expected:?}, got {carried:?} \
                 from {answered}"
            );
            let value = answered["SearchResults"][0]["Item"][row.attribute]["L"].clone();
            assert_eq!(
                value,
                held(row.dimensions),
                "case {case:?}: expected the coordinates the index holds ({}), got {value} \
                 from {answered}",
                held(row.dimensions)
            );
        }

        // And a projection that names everything EXCEPT the vector attribute retrieves no
        // embedding: naming the attribute is what retrieves it, not naming a projection.
        let answered = search(&router, row, json!("pk, note"), Value::Null).await;
        assert_eq!(
            answered["SearchResults"][0]["Item"].get(row.attribute),
            None,
            "case {:?} narrowed to \"pk, note\": expected no embedding, got {answered}",
            row.index
        );
    }
}

/// The table rule 2 meters over: two indexes of different `Dimensions` over the same items,
/// each item carrying both vectors and nothing else beyond its key.
const METER_TABLE: &str = "vector-meter-rule";

/// The dimensions the two metered indexes carry. The wide one is wide enough that the vector
/// one result is scored by passes the per-request floor on its own, and the narrow one
/// narrow enough that one result stays under it.
const NARROW_DIMENSIONS: usize = 64;
const WIDE_DIMENSIONS: usize = 256;

/// How many items the metered table holds, so a `TopK` asking for all of them does strictly
/// more work than one asking for a single hit.
const METERED_ITEMS: usize = 8;

/// Create the metered table and fill it.
async fn meter_table(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": METER_TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [
                index(
                    "narrow-idx",
                    "narrow",
                    NARROW_DIMENSIONS,
                    "COSINE",
                    json!({"ProjectionType": "ALL"}),
                ),
                index(
                    "wide-idx",
                    "wide",
                    WIDE_DIMENSIONS,
                    "COSINE",
                    json!({"ProjectionType": "ALL"}),
                ),
            ],
        }),
    )
    .await;
    ready(router, "the metered table", METER_TABLE).await;
    for at in 0..METERED_ITEMS {
        ok(
            router,
            "PutItem",
            json!({"TableName": METER_TABLE, "Item": {
                "pk": {"S": format!("i{at}")},
                "narrow": {"L": vector(NARROW_DIMENSIONS)},
                "wide": {"L": vector(WIDE_DIMENSIONS)},
            }}),
        )
        .await;
    }
}

/// The `VectorSearchRequestBytes` one metered search reports, or `None` where the answer
/// carries no `ConsumedCapacity` at all.
async fn metered(
    router: &MultiServiceRouter,
    index_name: &str,
    dimensions: usize,
    top_k: usize,
    projection: Option<&str>,
    requested: Option<&str>,
) -> (Value, Option<f64>) {
    let mut body = json!({
        "TableName": METER_TABLE,
        "IndexName": index_name,
        "SearchVector": vector(dimensions),
        "TopK": top_k,
    });
    if let Some(projection) = projection {
        body["ProjectionExpression"] = json!(projection);
    }
    if let Some(requested) = requested {
        body["ReturnConsumedCapacity"] = json!(requested);
    }
    let answered = ok(router, "SearchVectors", body).await;
    let figure = answered["ConsumedCapacity"]["VectorSearchRequestBytes"].as_f64();
    (answered, figure)
}

/// RULE 2: the figure grows with the results asked for, with the index's `Dimensions` and
/// with the attributes retrieved, and a search whose work is under the floor costs the
/// floor.
#[tokio::test]
async fn the_search_meter_grows_with_the_work_the_search_did() {
    let router = emulator();
    meter_table(&router).await;

    // One result off the narrow index, retrieving the key alone: under the floor, so it
    // costs the floor.
    let (answered, one_narrow) = metered(
        &router,
        "narrow-idx",
        NARROW_DIMENSIONS,
        1,
        Some("pk"),
        Some("TOTAL"),
    )
    .await;
    assert_eq!(
        one_narrow,
        Some(FLOOR_BYTES),
        "case \"one narrow hit, key only\": work under the floor costs the floor, got \
         {answered}"
    );

    // Every other row is read against it. Each pair differs in ONE thing.
    let (answered, every_narrow) = metered(
        &router,
        "narrow-idx",
        NARROW_DIMENSIONS,
        METERED_ITEMS,
        Some("pk"),
        Some("TOTAL"),
    )
    .await;
    let every_narrow = every_narrow.unwrap_or(f64::NAN);
    assert!(
        every_narrow > one_narrow.unwrap_or(f64::NAN),
        "case \"{METERED_ITEMS} narrow hits\": asking for more results scores more vectors, \
         so it costs more than the {one_narrow:?} of one hit, got {every_narrow} from \
         {answered}"
    );

    // The same TopK over an index of more dimensions: each vector scored is four bytes a
    // dimension, so a wider index costs more.
    let (answered, every_wide) = metered(
        &router,
        "wide-idx",
        WIDE_DIMENSIONS,
        METERED_ITEMS,
        Some("pk"),
        Some("TOTAL"),
    )
    .await;
    let every_wide = every_wide.unwrap_or(f64::NAN);
    assert!(
        every_wide > every_narrow,
        "case \"{METERED_ITEMS} wide hits\": {WIDE_DIMENSIONS} dimensions cost more than \
         {NARROW_DIMENSIONS}, got {every_wide} against {every_narrow} from {answered}"
    );

    // And the attributes RETRIEVED move it too: naming the embedding hands back four bytes a
    // dimension per result on top of what was scored.
    let (answered, with_vector) = metered(
        &router,
        "wide-idx",
        WIDE_DIMENSIONS,
        METERED_ITEMS,
        Some("pk, wide"),
        Some("TOTAL"),
    )
    .await;
    let with_vector = with_vector.unwrap_or(f64::NAN);
    assert!(
        with_vector > every_wide,
        "case \"{METERED_ITEMS} wide hits retrieving the embedding\": retrieving the vector \
         costs more than retrieving the key alone, got {with_vector} against {every_wide} \
         from {answered}"
    );
}

/// RULE 2, the shape half: the figure is reported only where the request asked for it, is
/// the same under `TOTAL` and `INDEXES`, and is the only member a search's
/// `ConsumedCapacity` carries — nothing was written, and the search read no table.
#[tokio::test]
async fn a_search_reports_only_the_vector_search_bytes_it_was_asked_for() {
    let router = emulator();
    meter_table(&router).await;

    for requested in [None, Some("NONE")] {
        let (answered, figure) = metered(
            &router,
            "narrow-idx",
            NARROW_DIMENSIONS,
            METERED_ITEMS,
            None,
            requested,
        )
        .await;
        assert_eq!(
            figure, None,
            "case {requested:?}: a request that did not ask for capacity is reported none, \
             got {answered}"
        );
        assert!(
            answered["ConsumedCapacity"].is_null(),
            "case {requested:?}: expected no ConsumedCapacity at all, got {answered}"
        );
    }

    let mut asked: Vec<f64> = Vec::new();
    for requested in ["TOTAL", "INDEXES"] {
        let (answered, figure) = metered(
            &router,
            "narrow-idx",
            NARROW_DIMENSIONS,
            METERED_ITEMS,
            None,
            Some(requested),
        )
        .await;
        let figure = figure.unwrap_or(f64::NAN);
        assert!(
            figure >= FLOOR_BYTES,
            "case {requested:?}: expected at least the {FLOOR_BYTES} floor, got {figure} \
             from {answered}"
        );
        let carried: Vec<String> = answered["ConsumedCapacity"]
            .as_object()
            .cloned()
            .unwrap_or_default()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            carried,
            vec!["VectorSearchRequestBytes".to_string()],
            "case {requested:?}: a search meters vector bytes and nothing else, got \
             {answered}"
        );
        asked.push(figure);
    }
    assert_eq!(
        asked.first(), asked.last(),
        "case \"TOTAL beside INDEXES\": the same search reports the same figure either way, \
         got {asked:?}"
    );
}
