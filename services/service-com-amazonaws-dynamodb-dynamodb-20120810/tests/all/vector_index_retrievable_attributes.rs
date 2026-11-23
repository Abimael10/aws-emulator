// SPDX-License-Identifier: CC0-1.0

//! What a `SearchVectors` through a vector index may hand back of an item.
//!
//! The rule under test: "only attributes projected into the vector index can be retrieved"
//! (`SearchVectors$ProjectionExpression`), and what is projected into an index is what its
//! own `Projection` copies there — "attributes that are copied (projected) from the table
//! into the vector index" (`VectorIndex$Projection`) — beside "the primary key attributes
//! and index key attributes, which are automatically projected" (`Projection`): the TABLE's
//! key and the attributes its `SearchSchema` is keyed and filtered by ("an attribute
//! projected into the vector index for filtering at the storage layer during search",
//! `SearchSchemaElement$SearchSchemaElementType`). The vector attribute reaches the index
//! through neither — it arrives in its own member (`VectorIndex$VectorAttribute`) as the
//! value the index is BUILT on — and a result "contains the projected attributes and a
//! similarity score" (`SearchVectors$SearchResults`), so a search never hands the vector
//! back: the score is what a result reports of it.
//!
//! The table spans what the rule generalises over: every `ProjectionType` the model
//! declares, an `INCLUDE` that names the vector attribute and one that does not, a search
//! schema that is absent and one that partitions and filters, a table keyed by a partition
//! key alone and one keyed by a composite key, and an attribute the item carries that no
//! index covers. Each row is read with and without a
//! `ProjectionExpression`, since a request may only narrow what the index projects — never
//! widen it.
//!
//! The items are read back through `GetItem` afterwards: narrowing what a search hands back
//! changes nothing the table stores.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Map, Value};

use crate::emulator::{emulator, ok, ACTIVE_DEADLINE, POLL_INTERVAL};

/// Poll `DescribeTable` until every vector index of `table` reports `ACTIVE` — the waiter
/// loop a client runs before searching one, since "the index must be in the ACTIVE state"
/// (`SearchVectors$IndexName`).
///
/// Bounded by the shared deadline: an index that never settles fails the test naming the
/// statuses it last reported rather than hanging.
async fn indexes_when_active(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    table: &str,
) {
    let started = std::time::Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < ACTIVE_DEADLINE {
        let described = ok(router, "DescribeTable", json!({"TableName": table})).await;
        last = described["Table"]["VectorIndexes"].clone();
        let statuses: Vec<&Value> = last
            .as_array()
            .map(|declared| {
                declared
                    .iter()
                    .map(|index| &index["IndexStatus"])
                    .collect()
            })
            .unwrap_or_default();
        if !statuses.is_empty() && statuses.iter().all(|status| *status == &json!("ACTIVE")) {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the vector indexes of {table} never all reached ACTIVE, last {last}")
}

/// The vector every item below carries, and the vector every search asks for: two
/// dimensions, so the index entry of an item stays small.
fn embedding() -> Value {
    json!([{"N": "1.0"}, {"N": "0.0"}])
}

/// One vector index declaration over the vector attribute `vec`.
fn index(name: &str, projection: Value, search_schema: Value) -> Value {
    let mut declared = json!({
        "IndexName": name,
        "VectorAttribute": {"AttributeName": "vec"},
        "Dimensions": 2,
        "DistanceFunction": "COSINE",
        "Projection": projection,
    });
    if !search_schema.is_null() {
        declared["SearchSchema"] = search_schema;
    }
    declared
}

/// The attribute names the first result of `response` carries, sorted.
fn reported_attributes(response: &Value) -> Vec<String> {
    let empty = Map::new();
    let mut names: Vec<String> = response["SearchResults"]
        .get(0)
        .and_then(|result| result["Item"].as_object())
        .unwrap_or(&empty)
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

/// One `SearchVectors` through `index` of `table`, pinning `tenant` when the index
/// partitions by it.
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

/// One row: the index, whether it partitions by `tenant`, and the attributes a search
/// through it reports — with no `ProjectionExpression`, and narrowed to `vec, note`.
struct Row {
    index: &'static str,
    partitioned: bool,
    whole: &'static [&'static str],
    narrowed: &'static [&'static str],
}

#[tokio::test]
async fn a_search_hands_back_only_what_the_vector_index_projects() {
    let router = emulator();
    let table = "vector-retrievable-attributes";
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": table,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "tenant", "AttributeType": "S"},
                {"AttributeName": "tier", "AttributeType": "S"},
            ],
            "VectorIndexes": [
                index("all", json!({"ProjectionType": "ALL"}), Value::Null),
                index("keys-only", json!({"ProjectionType": "KEYS_ONLY"}), Value::Null),
                index(
                    "include-note",
                    json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note"]}),
                    Value::Null,
                ),
                // An `INCLUDE` that names the vector attribute itself: naming it there does
                // not make it retrievable either, since the index is built on it rather than
                // handed it to copy.
                index(
                    "include-the-vector",
                    json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["vec"]}),
                    Value::Null,
                ),
                // A schema that partitions and filters: its attributes are projected into
                // the index whatever the `Projection` says.
                index(
                    "include-note-filtered",
                    json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note"]}),
                    json!([
                        {"AttributeName": "tenant", "SearchSchemaElementType": "HASH"},
                        {"AttributeName": "tier", "SearchSchemaElementType": "INLINE_FILTER"},
                    ]),
                ),
            ],
        }),
    )
    .await;
    indexes_when_active(&router, "the table carrying the vector indexes", table).await;

    let item = json!({
        "pk": {"S": "item-1"},
        "tenant": {"S": "t1"},
        "tier": {"S": "gold"},
        "note": {"S": "kept"},
        "extra": {"S": "unprojected"},
        "vec": {"L": embedding()},
    });
    ok(
        &router,
        "PutItem",
        json!({"TableName": table, "Item": item.clone()}),
    )
    .await;

    let rows: &[Row] = &[
        Row {
            index: "all",
            partitioned: false,
            whole: &["extra", "note", "pk", "tenant", "tier"],
            narrowed: &["note"],
        },
        Row {
            index: "keys-only",
            partitioned: false,
            whole: &["pk"],
            narrowed: &[],
        },
        Row {
            index: "include-note",
            partitioned: false,
            whole: &["note", "pk"],
            narrowed: &["note"],
        },
        Row {
            index: "include-the-vector",
            partitioned: false,
            whole: &["pk"],
            narrowed: &[],
        },
        Row {
            index: "include-note-filtered",
            partitioned: true,
            whole: &["note", "pk", "tenant", "tier"],
            narrowed: &["note"],
        },
    ];

    for row in rows {
        let whole = search(&router, table, row.index, row.partitioned, None).await;
        let reported = reported_attributes(&whole);
        assert_eq!(
            reported,
            row.whole.to_vec(),
            "case {:?}: expected a search to report {:?}, got {reported:?} from {whole}",
            row.index,
            row.whole
        );

        // A `ProjectionExpression` may only narrow what the index projects: a name it holds
        // nothing for reports nothing, which is also what a projection over a table's items
        // does for an attribute an item does not carry.
        let narrowed = search(&router, table, row.index, row.partitioned, Some("vec, note")).await;
        let reported = reported_attributes(&narrowed);
        assert_eq!(
            reported,
            row.narrowed.to_vec(),
            "case {:?} narrowed to \"vec, note\": expected {:?}, got {reported:?} from \
             {narrowed}",
            row.index,
            row.narrowed
        );
    }

    // What the TABLE holds is untouched by what a search of its indexes hands back.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": table, "Key": {"pk": {"S": "item-1"}}}),
    )
    .await;
    let mut stored: Vec<String> = read["Item"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .keys()
        .cloned()
        .collect();
    stored.sort();
    assert_eq!(
        stored,
        vec!["extra", "note", "pk", "tenant", "tier", "vec"],
        "case \"the stored item\": a search narrows only its own answer, got {read}"
    );
    assert_eq!(
        read["Item"]["extra"],
        item["extra"],
        "case \"the stored item\": the attribute no index projects is still stored, got {read}"
    );
}

#[tokio::test]
async fn a_composite_key_is_projected_into_a_vector_index_whole() {
    let router = emulator();
    let table = "vector-retrievable-composite-key";
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
            ],
            "VectorIndexes": [index(
                "keys-only",
                json!({"ProjectionType": "KEYS_ONLY"}),
                Value::Null,
            )],
        }),
    )
    .await;
    indexes_when_active(&router, "the table keyed by a composite key", table).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": table, "Item": {
            "pk": {"S": "item-1"},
            "sk": {"N": "7"},
            "note": {"S": "kept"},
            "vec": {"L": embedding()},
        }}),
    )
    .await;

    let answered = search(&router, table, "keys-only", false, None).await;
    let reported = reported_attributes(&answered);
    assert_eq!(
        reported,
        vec!["pk".to_string(), "sk".to_string()],
        "case \"a composite key\": both key attributes are projected automatically and \
         nothing else is, got {reported:?} from {answered}"
    );
}
