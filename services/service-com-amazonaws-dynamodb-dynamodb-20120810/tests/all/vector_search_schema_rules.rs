// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a vector index's SEARCH SCHEMA: what a declaration of one may
//! carry, and what a write to a table carrying one is held to.
//!
//! The rules pinned here:
//!
//! 1. "HASH - A partition key that partitions the vector index for independent scaling"
//!    (`SearchSchemaElement$SearchSchemaElementType`) — one partition key partitions an
//!    index, so a search schema declaring that role twice is refused as the
//!    `ValidationException` it is, wherever the index is declared (with the table, or
//!    through an `UpdateTable` that adds one), while a schema with one `HASH` element,
//!    with none, or with as many `INLINE_FILTER` elements as it carries is accepted;
//! 2. the `HASH` element is a KEY attribute of the index, so an item carrying the index's
//!    vector may not carry an empty `String` or `Binary` value under that name
//!    ("Attribute values of type String and Binary must have a length greater than zero
//!    if the attribute is used as a key attribute for a table or index" — the
//!    `PutItem$Item` doc), while an item that carries NO value for it states no partition
//!    of the index, is simply not held by it, and is written as it stands \u2014 and an
//!    `INLINE_FILTER` element, which is "optional", takes an empty value like any other
//!    projected attribute.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, and each rule is asserted over a
//! SET of inputs — which member declared the index, where in the schema the second `HASH`
//! element sits, which role the attribute plays and which emptiness the value has.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// One vector index declaration over `emb` with the search schema given.
fn vector_index(name: &str, search_schema: Value) -> Value {
    let mut index = json!({
        "IndexName": name,
        "VectorAttribute": {"AttributeName": "emb"},
        "Dimensions": 2,
        "DistanceFunction": "COSINE",
        "Projection": {"ProjectionType": "ALL"},
    });
    if !search_schema.is_null() {
        index["SearchSchema"] = search_schema;
    }
    index
}

/// One search-schema element.
fn element(attribute: &str, role: &str) -> Value {
    json!({"AttributeName": attribute, "SearchSchemaElementType": role})
}

/// AWS's fixed sentence for a schema declaring more elements of one ROLE than the index
/// admits — the same sentence a key schema's second `HASH` element earns, because a search
/// schema is what partitions and filters a vector index and the count exceeded is that
/// role's.
const PARTITION_KEYS_EXCEEDED_MESSAGE: &str =
    "One or more parameter values were invalid: The KeySchema exceeds the maximum allowed \
     number of HASH key attributes";

/// The attribute definitions a table declaring `search_schema` needs: `pk`, and every
/// attribute that schema names — "Some AttributeDefinitions are not used in any KeySchema"
/// refuses a definition no schema uses, and a search schema partitions and filters by the
/// attribute's VALUE, so its type has to be declared.
fn definitions(search_schema: &Value) -> Value {
    let mut declared = vec![json!({"AttributeName": "pk", "AttributeType": "S"})];
    for element in search_schema.as_array().into_iter().flatten() {
        let name = element["AttributeName"].as_str().unwrap_or_default();
        if name != "pk" {
            declared.push(json!({"AttributeName": name, "AttributeType": "S"}));
        }
    }
    Value::Array(declared)
}

/// A `CreateTable` body declaring `index` on an on-demand table.
fn create_with(table: &str, index: Value) -> Value {
    json!({
        "TableName": table,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": definitions(&index["SearchSchema"]),
        "VectorIndexes": [index],
    })
}

#[tokio::test]
async fn a_search_schema_may_declare_one_partition_key_at_most() {
    let router = emulator();

    // Rows differ in how many elements claim the partitioning role and where they sit: a
    // schema with one, with none, with only inline filters, and with the roles mixed is
    // accepted, while any schema naming HASH twice is refused — whichever positions the
    // two sit in.
    let cases: [(&str, Value, bool); 6] = [
        ("one HASH element", json!([element("tenant", "HASH")]), true),
        (
            "a HASH element beside an inline filter",
            json!([element("tenant", "HASH"), element("shard", "INLINE_FILTER")]),
            true,
        ),
        (
            "inline filters alone",
            json!([
                element("tenant", "INLINE_FILTER"),
                element("shard", "INLINE_FILTER")
            ]),
            true,
        ),
        ("no search schema at all", Value::Null, true),
        (
            "two HASH elements",
            json!([element("tenant", "HASH"), element("shard", "HASH")]),
            false,
        ),
        (
            "two HASH elements with an inline filter between them",
            json!([
                element("tenant", "HASH"),
                element("shard", "INLINE_FILTER"),
                element("pk", "HASH")
            ]),
            false,
        ),
    ];

    for (row, (case, search_schema, accepted)) in cases.into_iter().enumerate() {
        // Both members that declare a vector index are held to the one rule: the table's
        // own `VectorIndexes`, and the `Create` action of an `UpdateTable`.
        for member in ["CreateTable", "UpdateTable"] {
            let table = format!("SearchSchema{member}{row}");
            let index = vector_index("vec-idx", search_schema.clone());
            let (status, body) = match member {
                "CreateTable" => call(&router, "CreateTable", create_with(&table, index)).await,
                _ => {
                    ok(
                        &router,
                        "CreateTable",
                        json!({
                            "TableName": table,
                            "BillingMode": "PAY_PER_REQUEST",
                            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                            "AttributeDefinitions": [
                                {"AttributeName": "pk", "AttributeType": "S"}
                            ],
                        }),
                    )
                    .await;
                    describe_when_active(&router, case, &table).await;
                    call(
                        &router,
                        "UpdateTable",
                        json!({
                            "TableName": table,
                            "AttributeDefinitions": definitions(&index["SearchSchema"]),
                            "VectorIndexUpdates": [{"Create": index}],
                        }),
                    )
                    .await
                }
            };
            if accepted {
                assert_eq!(
                    status, 200,
                    "case {case:?} through {member}: expected the declaration to be accepted, \
                     got {status} with {body}"
                );
                continue;
            }
            assert_eq!(
                status, 400,
                "case {case:?} through {member}: expected the declaration to be refused, got \
                 {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?} through {member}: a schema the service will not build is the \
                 request's own defect, got {body}"
            );
            assert_eq!(
                message(&body),
                PARTITION_KEYS_EXCEEDED_MESSAGE,
                "case {case:?} through {member}: expected the sentence for a schema past the \
                 partitioning count, got {body}"
            );
        }
    }
}

/// One embedding of the two dimensions the index above declares.
fn embedding() -> Value {
    json!({"L": [{"N": "1"}, {"N": "0"}]})
}

#[tokio::test]
async fn a_write_may_not_leave_a_partitioning_search_schema_attribute_empty() {
    let router = emulator();
    let table = "SearchSchemaWrites";
    ok(
        &router,
        "CreateTable",
        create_with(
            table,
            vector_index(
                "vec-idx",
                json!([element("tenant", "HASH"), element("shard", "INLINE_FILTER")]),
            ),
        ),
    )
    .await;
    describe_when_active(&router, table, table).await;

    // Rows differ in the ROLE the schema gives the attribute, in what the item carries for
    // it — a value, no value at all, an empty string, an empty binary — and in whether the
    // item carries the index's vector at all.
    let cases: [(&str, Value, Option<&str>); 7] = [
        (
            "the partition key supplied",
            json!({"pk": {"S": "a"}, "tenant": {"S": "t1"}, "emb": embedding()}),
            None,
        ),
        (
            "the partition key left out",
            json!({"pk": {"S": "b"}, "emb": embedding()}),
            None,
        ),
        (
            "the partition key left out of an item with no vector",
            json!({"pk": {"S": "c"}}),
            None,
        ),
        (
            "an empty string under the partition key",
            json!({"pk": {"S": "d"}, "tenant": {"S": ""}, "emb": embedding()}),
            Some("string"),
        ),
        (
            "an empty binary under the partition key",
            json!({"pk": {"S": "e"}, "tenant": {"B": ""}, "emb": embedding()}),
            Some("binary"),
        ),
        (
            "an empty string under an inline filter",
            json!({"pk": {"S": "f"}, "tenant": {"S": "t1"}, "shard": {"S": ""},
                   "emb": embedding()}),
            None,
        ),
        (
            "an empty string under the partition key of an item with no vector",
            json!({"pk": {"S": "g"}, "tenant": {"S": ""}}),
            None,
        ),
    ];

    for (case, item, refused) in cases {
        let (status, body) = call(
            &router,
            "PutItem",
            json!({"TableName": table, "Item": item}),
        )
        .await;
        match refused {
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the write to be accepted, got {status} with {body}"
            ),
            Some(kind) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the write to be refused, got {status} with {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "ValidationException",
                    "case {case:?}: a value no index key may hold is the request's own defect, \
                     got {body}"
                );
                assert_eq!(
                    message(&body),
                    format!(
                        "One or more parameter values are not valid. A value specified for a \
                         secondary index key is not supported. The AttributeValue for a key \
                         attribute cannot contain an empty {kind} value. IndexName: vec-idx, \
                         IndexKey: tenant"
                    ),
                    "case {case:?}: expected the sentence for an empty index key value, got \
                     {body}"
                );
            }
        }
    }

    // An item the index could not partition was written all the same, and the index simply
    // does not hold it: a search of the index reports the items that named a partition and
    // none of the others.
    let searched = ok(
        &router,
        "SearchVectors",
        json!({
            "TableName": table,
            "IndexName": "vec-idx",
            "SearchVector": [{"N": "1"}, {"N": "0"}],
            "TopK": 10,
            "SearchConditionExpression": "tenant = :t",
            "ExpressionAttributeValues": {":t": {"S": "t1"}},
        }),
    )
    .await;
    let mut keys: Vec<String> = searched["SearchResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|result| {
            result["Item"]["pk"]
                .get("S")
                .and_then(Value::as_str)
                .unwrap_or("<no pk>")
                .to_string()
        })
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["a".to_string(), "f".to_string()],
        "the index holds the items that named its partition and no others, got {searched}"
    );
    // ...and the item that named none is still stored, which is what makes the write an
    // acceptance rather than a silent drop.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": table, "Key": {"pk": {"S": "b"}}}),
    )
    .await;
    assert_eq!(
        read["Item"]["emb"],
        embedding(),
        "the item the index does not hold is stored as it was written, got {read}"
    );
}
