// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an `IndexName` resolves to on a table that carries a VECTOR
//! index.
//!
//! The rule pinned here: a vector index is SEARCHED — `SearchVectors` "performs a vector
//! similarity search on a vector index" and takes its own `IndexName` — so an operation
//! that reads a secondary index cannot be served from one, and a request naming it is
//! refused as the index TYPE it resolved to rather than told the table has no such index:
//! the table does have it, so reporting it absent would send a caller looking for a
//! resource that is there. A name the table carries no index for at all keeps the
//! not-found-index sentence, and a secondary index of the same table is read as it always
//! was.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, and the rule is asserted over a
//! SET of inputs — both readers, and all three kinds of name a request can send.

use serde_json::json;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row reads.
const TABLE: &str = "VectorIndexIsNotQueryable";

/// The vector index of that table, and the secondary index beside it.
const VECTOR_INDEX: &str = "vec-idx";
const SECONDARY_INDEX: &str = "gsi-idx";

/// Create the table: one vector index over `emb`, one global secondary index over
/// `tenant`, and one item reaching both.
async fn table(router: &smithy_server_runtime::MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "tenant", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": SECONDARY_INDEX,
                "KeySchema": [{"AttributeName": "tenant", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "VectorIndexes": [{
                "IndexName": VECTOR_INDEX,
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": 2,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(router, TABLE, TABLE).await;
    ok(
        router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {
                "pk": {"S": "i1"},
                "tenant": {"S": "t1"},
                "emb": {"L": [{"N": "1"}, {"N": "0"}]},
            },
        }),
    )
    .await;
}

/// What each reader answers for one `IndexName`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The index is one this reader is served from.
    Served,
    /// The name resolved to an index of a kind this reader cannot read.
    WrongType,
    /// The table carries no index by that name.
    Absent,
}

#[tokio::test]
async fn a_vector_index_is_not_a_secondary_index_a_read_can_be_served_from() {
    let router = emulator();
    table(&router).await;

    // Rows differ in the kind of name the request sends and in the reader that sends it:
    // both readers answer each kind the same way, and each refusal names what is wrong with
    // the name rather than what is wrong with the table.
    let names: [(&str, &str, Expected); 3] = [
        ("a vector index", VECTOR_INDEX, Expected::WrongType),
        ("a secondary index", SECONDARY_INDEX, Expected::Served),
        ("no index of the table", "nowhere-idx", Expected::Absent),
    ];
    for (case, index, expected) in names {
        for operation in ["Query", "Scan"] {
            let mut request = json!({"TableName": TABLE, "IndexName": index});
            if operation == "Query" {
                request["KeyConditionExpression"] = json!("tenant = :t");
                request["ExpressionAttributeValues"] = json!({":t": {"S": "t1"}});
            }
            let (status, body) = call(&router, operation, request).await;
            match expected {
                Expected::Served => {
                    assert_eq!(
                        status, 200,
                        "case {case:?} through {operation}: expected the read to be served, got \
                         {status} with {body}"
                    );
                    assert_eq!(
                        body["Count"], json!(1),
                        "case {case:?} through {operation}: expected the indexed item, got {body}"
                    );
                }
                Expected::WrongType | Expected::Absent => {
                    assert_eq!(
                        status, 400,
                        "case {case:?} through {operation}: expected the read to be refused, got \
                         {status} with {body}"
                    );
                    assert_eq!(
                        error_code(&body),
                        "ValidationException",
                        "case {case:?} through {operation}: the name the request sent is its own \
                         defect, got {body}"
                    );
                    let expected_message = if expected == Expected::WrongType {
                        format!("{operation} operation not supported on this index type.")
                    } else {
                        format!("The table does not have the specified index: {index}")
                    };
                    assert_eq!(
                        message(&body),
                        expected_message,
                        "case {case:?} through {operation}: expected the sentence for that name, \
                         got {body}"
                    );
                }
            }
        }
    }

    // ...and the vector index still serves the operation it IS for, so the refusals above
    // are about which reader was asked and not about the index.
    let searched = ok(
        &router,
        "SearchVectors",
        json!({
            "TableName": TABLE,
            "IndexName": VECTOR_INDEX,
            "SearchVector": [{"N": "1"}, {"N": "0"}],
            "TopK": 5,
        }),
    )
    .await;
    assert_eq!(
        searched["SearchResults"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "the vector index serves the search it is for, got {searched}"
    );
    assert_eq!(
        searched["SearchResults"][0]["Item"]["pk"],
        json!({"S": "i1"}),
        "the search reports the item the index holds, got {searched}"
    );
    // A `DescribeTable` reports the index the readers refused, which is why reporting it
    // absent would contradict the table's own description.
    let described = ok(&router, "DescribeTable", json!({"TableName": TABLE})).await;
    let reported: Vec<&str> = described["Table"]["VectorIndexes"]
        .as_array()
        .map(|indexes| {
            indexes
                .iter()
                .filter_map(|index| index["IndexName"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        reported,
        vec![VECTOR_INDEX],
        "the table reports the vector index a read cannot be served from, got {described}"
    );
    // ...and it is no part of the secondary-index lists a reader resolves names from.
    assert_eq!(
        described["Table"]["GlobalSecondaryIndexes"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "a vector index is not a secondary index of the table, got {described}"
    );
}
