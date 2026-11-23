// SPDX-License-Identifier: CC0-1.0

//! Which KEY SCHEMA a read's `FilterExpression` is held to.
//!
//! RULE: a filter is decided over the entries the read WALKS, so the key attributes in force
//! for its document paths are the ones of the key schema that read ADDRESSES — the table's
//! own key schema, plus the key schema of the secondary index the request names in
//! `IndexName`. A path that reaches INSIDE one of those addresses a location no key attribute
//! can hold and is refused naming it; an attribute that is only the key of an index the read
//! does NOT go through is an ordinary attribute of that walk, so a path reaching inside it is
//! served and simply matches nothing.
//!
//! The rows below differ in the key attribute the path dereferences (a partition key, a sort
//! key, a global index's hash and range keys, a local index's sort key), in the STEP that
//! dereferences it (a `.` name step, a `[]` index step, and a name behind an
//! `ExpressionAttributeNames` alias), in the OPERATION that filters (`Scan` and `Query`) and
//! in which index — none, the global one, the local one — the read is pointed at.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's front
//! doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row addresses: a composite key schema, one global secondary index keyed by
/// `gpk`/`gsk` and one local secondary index keyed by `lsk`, so a row can name a key attribute
/// of each kind.
const TABLE: &str = "FilterIndexKeyPaths";

/// The global secondary index a row either reads through or leaves unread.
const GLOBAL_INDEX: &str = "ByGpk";

/// The local secondary index, for the same two readings.
const LOCAL_INDEX: &str = "ByLsk";

/// How many items the table holds, which is what an accepted filter is evaluated over.
const ITEMS: usize = 3;

/// AWS's fixed sentence for a path that reaches inside a key attribute, with the key attribute
/// it dereferenced appended.
fn not_a_scalar_key(attribute: &str) -> String {
    format!(
        "Key attributes must be scalars; list random access '[]' and map lookup '.' are not \
         allowed: Key: {attribute}"
    )
}

/// AWS's sentence for a filter that speaks of a key attribute of the key schema the read
/// addresses at all, whole or dereferenced (`Query$FilterExpression`: "A FilterExpression does
/// not allow key attributes"), as the stated member is named in it.
fn not_a_filter_attribute(attribute: &str) -> String {
    format!(
        "Filter Expression can only contain non-primary key attributes: Primary key \
         attribute: {attribute}"
    )
}

/// Create [`TABLE`], wait out the documented `CREATING` -> `ACTIVE` transition and write
/// [`ITEMS`] items, each carrying every index key attribute as the SCALAR its definition
/// declares plus one map attribute a path may legitimately reach into.
async fn seeded(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "gpk", "AttributeType": "S"},
                {"AttributeName": "gsk", "AttributeType": "N"},
                {"AttributeName": "lsk", "AttributeType": "S"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": GLOBAL_INDEX,
                    "KeySchema": [
                        {"AttributeName": "gpk", "KeyType": "HASH"},
                        {"AttributeName": "gsk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
            "LocalSecondaryIndexes": [
                {
                    "IndexName": LOCAL_INDEX,
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "lsk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
        }),
    )
    .await;
    describe_when_active(router, "seed the indexed table", TABLE).await;
    for sort in 1..=ITEMS {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {
                    "pk": {"S": "p"},
                    "sk": {"N": sort.to_string()},
                    "gpk": {"S": "g"},
                    "gsk": {"N": "1"},
                    "lsk": {"S": format!("l{sort}")},
                    "note": {"M": {"leaf": {"S": "n"}}},
                },
            }),
        )
        .await;
    }
}

/// One filtering read, and what the key schema it addresses makes of the path it states.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    /// The whole message the row earns, or `None` for a read the rule serves.
    refused: Option<String>,
}

#[tokio::test]
async fn a_filter_path_is_held_to_the_key_schema_the_read_addresses() {
    let router = emulator();
    seeded(&router).await;

    let rows: Vec<Row> = vec![
        // A read of the TABLE: its own key attributes are in force, whichever step
        // dereferences them.
        Row {
            case: "a scan's filter reaching into the partition key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "pk.leaf = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            refused: Some(not_a_scalar_key("pk")),
        },
        Row {
            case: "a scan's filter indexing the sort key as a list",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "sk[0] = :n",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            refused: Some(not_a_scalar_key("sk")),
        },
        Row {
            case: "a scan's filter reaching into the partition key behind an alias",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "#alias.leaf = :v",
                            "ExpressionAttributeNames": {"#alias": "pk"},
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            refused: Some(not_a_scalar_key("pk")),
        },
        // A read THROUGH an index: that index's key attributes are in force too.
        Row {
            case: "a scan through the global index whose filter reaches into its hash key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL_INDEX,
                            "FilterExpression": "gpk.leaf = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            refused: Some(not_a_scalar_key("gpk")),
        },
        Row {
            case: "a scan through the global index whose filter indexes its range key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL_INDEX,
                            "FilterExpression": "gsk[1] = :n",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            refused: Some(not_a_scalar_key("gsk")),
        },
        Row {
            case: "a scan through the local index whose filter reaches into its sort key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "IndexName": LOCAL_INDEX,
                            "FilterExpression": "lsk.leaf = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            refused: Some(not_a_scalar_key("lsk")),
        },
        // And the other reading: an attribute that is only ANOTHER index's key is an
        // ordinary attribute of the walk this read makes.
        Row {
            case: "a scan of the table whose filter reaches into a global index's hash key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "gpk.leaf = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            refused: None,
        },
        Row {
            case: "a scan of the table whose filter indexes a global index's range key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "gsk[0] = :n",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            refused: None,
        },
        Row {
            case: "a scan of the table whose filter reaches into a local index's sort key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "lsk.leaf = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            refused: None,
        },
        Row {
            case: "the same path behind an alias",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "#alias.leaf = :v",
                            "ExpressionAttributeNames": {"#alias": "gpk"},
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            refused: None,
        },
        Row {
            case: "a query of the table whose filter reaches into a global index's hash key",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "gpk.leaf = :v",
                            "ExpressionAttributeValues": {":p": {"S": "p"},
                                                          ":v": {"S": "x"}}}),
            refused: None,
        },
        Row {
            case: "a query of the table whose filter reaches into a local index's sort key",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "lsk.leaf = :v",
                            "ExpressionAttributeValues": {":p": {"S": "p"},
                                                          ":v": {"S": "x"}}}),
            refused: None,
        },
        Row {
            case: "a query through the global index filtering into the local index's sort key",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "IndexName": GLOBAL_INDEX,
                            "KeyConditionExpression": "gpk = :g",
                            "FilterExpression": "lsk.leaf = :v",
                            "ExpressionAttributeValues": {":g": {"S": "g"},
                                                          ":v": {"S": "x"}}}),
            refused: None,
        },
    ];

    for row in &rows {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        match row.refused.as_deref() {
            Some(stated) => assert_eq!(
                (status, error_code(&body).as_str(), message(&body).as_str()),
                (400, "ValidationException", stated),
                "case {:?} on {}: expected the refusal {stated:?}, got {status} with {body}",
                row.case,
                row.operation
            ),
            None => assert_eq!(
                status, 200,
                "case {:?} on {}: a path into an attribute no key schema of this read covers \
                 is served, got {status} with {body}",
                row.case, row.operation
            ),
        }
    }
}

#[tokio::test]
async fn a_served_filter_path_into_a_scalar_attribute_matches_nothing() {
    let router = emulator();
    seeded(&router).await;

    // The accepted filter is EVALUATED: the index key attribute it reaches into holds the
    // scalar its definition declares, so the path addresses a location no item has and the
    // scan reports no match while still having walked every item.
    let scanned = ok(
        &router,
        "Scan",
        json!({"TableName": TABLE, "FilterExpression": "gpk.leaf = :v",
               "ExpressionAttributeValues": {":v": {"S": "x"}}}),
    )
    .await;
    assert_eq!(
        (scanned["Count"].clone(), scanned["ScannedCount"].clone()),
        (json!(0), json!(ITEMS)),
        "a path into a scalar index key matches nothing over the {ITEMS} items walked, got \
         {scanned}"
    );

    // The CONTROL: the same read filtering on a path into a MAP attribute matches every item,
    // so the zero above is the filter's own answer rather than a read that returned nothing.
    let matched = ok(
        &router,
        "Scan",
        json!({"TableName": TABLE, "FilterExpression": "note.leaf = :n",
               "ExpressionAttributeValues": {":n": {"S": "n"}}}),
    )
    .await;
    assert_eq!(
        (matched["Count"].clone(), matched["ScannedCount"].clone()),
        (json!(ITEMS), json!(ITEMS)),
        "a path into a map attribute matches every item, got {matched}"
    );
}

#[tokio::test]
async fn a_query_filter_naming_a_key_of_the_schema_it_reads_is_refused_as_a_key_attribute() {
    let router = emulator();
    seeded(&router).await;

    // A `Query`'s filter may not speak of a key attribute of the schema it reads AT ALL, so
    // the key attributes in force there are refused by that rule, whether the path names one
    // whole or reaches inside it — which is why the scalar-path rule above only ever has the
    // keys of an index this read does not go through left to serve.
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a query naming the sort key whole",
            json!({"TableName": TABLE,
                   "KeyConditionExpression": "pk = :p",
                   "FilterExpression": "sk = :n",
                   "ExpressionAttributeValues": {":p": {"S": "p"}, ":n": {"N": "1"}}}),
            not_a_filter_attribute("sk"),
        ),
        (
            "a query reaching into the sort key",
            json!({"TableName": TABLE,
                   "KeyConditionExpression": "pk = :p",
                   "FilterExpression": "sk.leaf = :n",
                   "ExpressionAttributeValues": {":p": {"S": "p"}, ":n": {"N": "1"}}}),
            not_a_filter_attribute("sk"),
        ),
        (
            "a query through the global index reaching into that index's hash key",
            json!({"TableName": TABLE,
                   "IndexName": GLOBAL_INDEX,
                   "KeyConditionExpression": "gpk = :g",
                   "FilterExpression": "gpk.leaf = :v",
                   "ExpressionAttributeValues": {":g": {"S": "g"}, ":v": {"S": "x"}}}),
            not_a_filter_attribute("gpk"),
        ),
    ];

    for (case, request, stated) in &rows {
        let (status, body) = call(&router, "Query", request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", stated.as_str()),
            "case {case:?}: expected the refusal {stated:?}, got {status} with {body}"
        );
    }
}
