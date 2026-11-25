// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the STATUS a vector index whose delete `UpdateTable` has accepted
//! is reported in, and for when its entry stops being reported at all.
//!
//! The rule pinned here: removing a vector index is asynchronous work on the table, so the
//! table keeps reporting the index — with `IndexStatus` `DELETING`, "the index is being
//! deleted" (`VectorIndexDescription$IndexStatus`) — from the call that accepts the delete
//! until the clock makes the removal due, and only then does the entry disappear. Which is
//! what lets a client that reads the response to learn the new index state tell "delete
//! requested" from "delete finished" and from "the index never existed", and the index
//! serves no search from the moment the delete is accepted: only an `ACTIVE` index is one
//! of the sources a table can be searched by.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, and the rule is asserted over a SET
//! of inputs — how many vector indexes the table carries, which position of its list the
//! deleted index sits in, whether the table holds items for the index to have read, and
//! whether the index had settled when the delete arrived — rather than one example.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok, table_when};

/// One vector index body over the `emb` attribute, with four dimensions.
fn vector_index(name: &str, distance_function: &str) -> Value {
    json!({
        "IndexName": name,
        "VectorAttribute": {"AttributeName": "emb"},
        "Dimensions": 4,
        "DistanceFunction": distance_function,
        "Projection": {"ProjectionType": "ALL"},
    })
}

/// One embedding, in the DynamoDB list format a vector attribute is written in.
fn embedding() -> Value {
    json!([{"N": "1"}, {"N": "0"}, {"N": "0"}, {"N": "0"}])
}

/// Create one on-demand table keyed `pk`, carrying `indexes`, and write `items` items to
/// it; the table is `ACTIVE` when this returns.
async fn table_with(
    router: &MultiServiceRouter,
    name: &str,
    indexes: Value,
    items: usize,
) -> Value {
    let mut request = json!({
        "TableName": name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
    });
    // A table declaring no vector index states the member nowhere: the list is "one or
    // more vector indexes" (`CreateTable$VectorIndexes`), so an empty one declares nothing.
    if indexes.as_array().is_some_and(|indexes| !indexes.is_empty()) {
        request["VectorIndexes"] = indexes;
    }
    ok(router, "CreateTable", request).await;
    let described = describe_when_active(router, name, name).await;
    for item in 0..items {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": name,
                "Item": {
                    "pk": {"S": format!("k{item}")},
                    "emb": {"L": embedding()},
                },
            }),
        )
        .await;
    }
    described
}

/// Every vector index a description reports, as (name, status) pairs in name order.
fn reported(description: &Value) -> Vec<(String, String)> {
    let mut indexes: Vec<(String, String)> = description["VectorIndexes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|index| {
            (
                index["IndexName"].as_str().unwrap_or_default().to_string(),
                index["IndexStatus"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    indexes.sort();
    indexes
}

/// The description of the vector index `index`, or `Value::Null` when the description
/// reports no index by that name.
fn entry(description: &Value, index: &str) -> Value {
    description["VectorIndexes"]
        .as_array()
        .and_then(|indexes| {
            indexes
                .iter()
                .find(|reported| reported["IndexName"] == json!(index))
        })
        .cloned()
        .unwrap_or(Value::Null)
}

/// `SearchVectors` through `index` for the embedding the items carry.
async fn search(router: &MultiServiceRouter, table: &str, index: &str) -> (u16, Value) {
    call(
        router,
        "SearchVectors",
        json!({
            "TableName": table,
            "IndexName": index,
            "SearchVector": embedding(),
            "TopK": 4,
        }),
    )
    .await
}

/// The `UpdateTable` body deleting the vector index `index`.
fn delete_request(table: &str, index: &str) -> Value {
    json!({
        "TableName": table,
        "VectorIndexUpdates": [{"Delete": {"IndexName": index}}],
    })
}

#[tokio::test]
async fn an_accepted_vector_index_delete_is_reported_deleting_until_the_removal_falls_due() {
    // Rows differ along what the rule generalises over: how many vector indexes the table
    // carries, which position of its list the deleted one sits in, the distance function
    // it was created with, and whether the table holds items the index had to read.
    let cases: [(&str, Value, &str, &[&str], usize); 4] = [
        (
            "the only index of an empty table",
            json!([vector_index("idx-only", "COSINE")]),
            "idx-only",
            &[],
            0,
        ),
        (
            "the only index of a populated table",
            json!([vector_index("idx-only", "EUCLIDEAN")]),
            "idx-only",
            &[],
            2,
        ),
        (
            "the first of three",
            json!([
                vector_index("idx-a", "COSINE"),
                vector_index("idx-b", "DOT_PRODUCT"),
                vector_index("idx-c", "EUCLIDEAN"),
            ]),
            "idx-a",
            &["idx-b", "idx-c"],
            1,
        ),
        (
            "the last of three",
            json!([
                vector_index("idx-a", "COSINE"),
                vector_index("idx-b", "DOT_PRODUCT"),
                vector_index("idx-c", "EUCLIDEAN"),
            ]),
            "idx-c",
            &["idx-a", "idx-b"],
            1,
        ),
    ];

    for (position, (case, indexes, deleted, survivors, items)) in cases.into_iter().enumerate() {
        let router = emulator();
        let table = format!("VectorDeleting{position}");
        let created = table_with(&router, &table, indexes, items).await;
        let declared = entry(&created, deleted);

        let answered = ok(&router, "UpdateTable", delete_request(&table, deleted)).await;
        let described = &answered["TableDescription"];
        // The work outlives the call, so the table is UPDATING...
        assert_eq!(
            described["TableStatus"],
            json!("UPDATING"),
            "case {case:?}: removing an index is work that outlives the call, got {answered}"
        );
        // ... and the index is still LISTED, in the status that says what is happening to
        // it, beside the siblings this delete left alone.
        let mut expected: Vec<(String, String)> = survivors
            .iter()
            .map(|name| ((*name).to_string(), "ACTIVE".to_string()))
            .collect();
        expected.push((deleted.to_string(), "DELETING".to_string()));
        expected.sort();
        assert_eq!(
            reported(described),
            expected,
            "case {case:?}: expected {deleted} reported DELETING beside its siblings, got \
             {answered}"
        );
        // The entry is the stored index, not a stub: every member but its status is the one
        // the create answered, so a client polling it reads the index it asked about.
        let deleting = entry(described, deleted);
        for member in [
            "IndexName",
            "IndexArn",
            "Dimensions",
            "DistanceFunction",
            "Projection",
            "VectorAttribute",
        ] {
            assert_eq!(
                deleting[member],
                declared[member],
                "case {case:?}: the DELETING entry must carry the stored {member}, got \
                 {deleting}"
            );
        }

        // From that moment the index is not addressable: a search through it is the request
        // mistake naming the index, not a missing table.
        let (status, response) = search(&router, &table, deleted).await;
        assert_eq!(
            (status, error_code(&response), message(&response)),
            (
                400,
                "ValidationException".to_string(),
                format!("The table does not have the specified index: {deleted}")
            ),
            "case {case:?}: an index being deleted serves no search, got {status} with \
             {response}"
        );

        // And the entry disappears only once the removal the clock owed falls due, which
        // is when the table's own update settles.
        let settled = table_when(&router, case, crate::emulator::REGION, &table, |table| {
            table["TableStatus"] == json!("ACTIVE") && entry(table, deleted) == Value::Null
        })
        .await;
        let mut gone: Vec<(String, String)> = survivors
            .iter()
            .map(|name| ((*name).to_string(), "ACTIVE".to_string()))
            .collect();
        gone.sort();
        assert_eq!(
            reported(&settled),
            gone,
            "case {case:?}: the settled table carries exactly the surviving indexes, got \
             {settled}"
        );
        // Every survivor is still searchable, so the delete reached exactly one index.
        for survivor in survivors {
            let (status, response) = search(&router, &table, survivor).await;
            assert_eq!(
                status, 200,
                "case {case:?}: index {survivor} must still serve SearchVectors, got {status} \
                 with {response}"
            );
        }
    }
}

#[tokio::test]
async fn cancelling_a_vector_index_build_reports_the_index_deleting_too() {
    // The status an accepted delete reports is the DELETE's, whatever the index was doing
    // when it arrived: an index still reading the items the table already held is cancelled
    // by the delete, and is reported on its way out exactly as a settled one is.
    let router = emulator();
    let table = "VectorDeletingWhileBuilding";
    table_with(&router, table, json!([]), 2).await;
    ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": table,
            "VectorIndexUpdates": [{"Create": vector_index("idx-added", "COSINE")}],
        }),
    )
    .await;
    // The table settles while the index it was given keeps reading those items, which is
    // the phase a delete of it is served in.
    describe_when_active(&router, table, table).await;

    let answered = ok(&router, "UpdateTable", delete_request(table, "idx-added")).await;
    assert_eq!(
        reported(&answered["TableDescription"]),
        vec![("idx-added".to_string(), "DELETING".to_string())],
        "the cancelled build is reported as the deletion it now is, got {answered}"
    );
    let settled = table_when(&router, table, crate::emulator::REGION, table, |table| {
        table["TableStatus"] == json!("ACTIVE") && entry(table, "idx-added") == Value::Null
    })
    .await;
    assert_eq!(
        reported(&settled),
        Vec::new(),
        "the cancelled index is gone once the removal falls due, got {settled}"
    );
}
