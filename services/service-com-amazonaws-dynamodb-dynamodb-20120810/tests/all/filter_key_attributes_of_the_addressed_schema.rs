// SPDX-License-Identifier: CC0-1.0

//! Which key attributes a read's filter may not speak of.
//!
//! RULE: "A FilterExpression does not allow key attributes. You cannot define a filter
//! expression based on a partition key or a sort key" (`Query$FilterExpression`) — and the
//! key attributes in force are the ones of the KEY SCHEMA THE READ IS ADDRESSED THROUGH:
//! the secondary index named in `IndexName`, and the table's own key schema when the
//! request names no index. An attribute that is only a key of a schema the read does NOT go
//! through is an ordinary attribute of that walk, so a filter may narrow on it and the read
//! answers the narrowed page. A `Scan` states no key condition at all, so nothing is in
//! force for it.
//!
//! The refusal names the member the request filtered through and the offending attribute,
//! read in the order the key schema DECLARES its elements, so a filter naming more than one
//! of them is reported for the partition key it named.
//!
//! The rows span the schema the read addresses (the table, a global index, a local index),
//! which key attribute the filter names (the table's partition and sort keys, the index's
//! own hash and range keys, both at once), the generation the filter was stated in (the
//! expression member and the legacy map) and the operation (`Query` and `Scan`).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row reads: a composite key schema, a global index with a key of its own
/// and a local index whose sort key is its own.
const TABLE: &str = "FilterKeySchemas";

/// The global secondary index, keyed by attributes the table is not keyed by.
const GLOBAL: &str = "ByGpk";

/// The local secondary index: the table's partition key, and a sort key of its own.
const LOCAL: &str = "ByLsk";

/// How many items the table holds, which is what a served filter is evaluated over.
const ITEMS: i64 = 3;

/// The sort-key value the narrowing rows filter for.
const NARROWED: i64 = 2;

/// AWS's sentence for a filter that speaks of a key attribute of the schema the read
/// addresses, as the member that carried the filter is named in it.
fn not_a_filter_attribute(member: &str, attribute: &str) -> String {
    format!("{member} can only contain non-primary key attributes: Primary key attribute: {attribute}")
}

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
            "GlobalSecondaryIndexes": [{
                "IndexName": GLOBAL,
                "KeySchema": [
                    {"AttributeName": "gpk", "KeyType": "HASH"},
                    {"AttributeName": "gsk", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "LocalSecondaryIndexes": [{
                "IndexName": LOCAL,
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "lsk", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(router, "seed the indexed table", TABLE).await;
    for sort in 1..=ITEMS {
        ok(
            router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {
                "pk": {"S": "p"},
                "sk": {"N": sort.to_string()},
                "gpk": {"S": "g"},
                "gsk": {"N": "1"},
                "lsk": {"S": format!("l{sort}")},
            }}),
        )
        .await;
    }
}

/// The value one placeholder stands for, so each row can bind exactly the placeholders its
/// own expressions dereference — a value bound and never read is refused by its own rule.
fn value(placeholder: &str) -> Value {
    match placeholder {
        ":p" => json!({"S": "p"}),
        ":n" => json!({"N": NARROWED.to_string()}),
        ":g" => json!({"S": "g"}),
        ":gs" => json!({"N": "1"}),
        _ => json!({"S": "l1"}),
    }
}

/// The `ExpressionAttributeValues` map binding exactly `placeholders`.
fn values(placeholders: &[&str]) -> Value {
    let mut bound = serde_json::Map::new();
    for placeholder in placeholders {
        bound.insert((*placeholder).to_string(), value(placeholder));
    }
    Value::Object(bound)
}

/// One filtering read: the request, and what the schema it addresses makes of the filter.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    /// The whole message the row earns, or `None` for a read the rule serves — which must
    /// answer the one item its filter narrows to.
    refused: Option<String>,
}

fn rows() -> Vec<Row> {
    vec![
        // A read of the TABLE: its own key attributes are in force.
        Row {
            case: "a query of the table filtering on its sort key",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "sk = :n",
                            "ExpressionAttributeValues": values(&[":p", ":n"])}),
            refused: Some(not_a_filter_attribute("Filter Expression", "sk")),
        },
        Row {
            case: "a query of the table filtering on its partition key",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "pk = :p",
                            "ExpressionAttributeValues": values(&[":p"])}),
            refused: Some(not_a_filter_attribute("Filter Expression", "pk")),
        },
        Row {
            case: "a query of the table filtering on both of its key attributes",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "sk = :n AND pk = :p",
                            "ExpressionAttributeValues": values(&[":p", ":n"])}),
            // The partition key comes first in the schema, whichever order the filter
            // wrote the two.
            refused: Some(not_a_filter_attribute("Filter Expression", "pk")),
        },
        // A read through the GLOBAL index: ITS key attributes are in force, and the
        // table's own are ordinary attributes of that walk.
        Row {
            case: "a global index query filtering on the table's sort key",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL,
                            "KeyConditionExpression": "gpk = :g",
                            "FilterExpression": "sk = :n",
                            "ExpressionAttributeValues": values(&[":g", ":n"])}),
            refused: None,
        },
        Row {
            case: "a global index query filtering on both of the table's key attributes",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL,
                            "KeyConditionExpression": "gpk = :g",
                            "FilterExpression": "pk = :p AND sk = :n",
                            "ExpressionAttributeValues": values(&[":g", ":p", ":n"])}),
            refused: None,
        },
        Row {
            case: "a global index query filtering on its own hash key",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL,
                            "KeyConditionExpression": "gpk = :g",
                            "FilterExpression": "gpk = :g",
                            "ExpressionAttributeValues": values(&[":g"])}),
            refused: Some(not_a_filter_attribute("Filter Expression", "gpk")),
        },
        Row {
            case: "a global index query filtering on its own range key",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL,
                            "KeyConditionExpression": "gpk = :g",
                            "FilterExpression": "gsk = :gs",
                            "ExpressionAttributeValues": values(&[":g", ":gs"])}),
            refused: Some(not_a_filter_attribute("Filter Expression", "gsk")),
        },
        Row {
            case: "a global index query filtering on both of its own key attributes",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL,
                            "KeyConditionExpression": "gpk = :g",
                            "FilterExpression": "gsk = :gs AND gpk = :g",
                            "ExpressionAttributeValues": values(&[":g", ":gs"])}),
            refused: Some(not_a_filter_attribute("Filter Expression", "gpk")),
        },
        // A read through the LOCAL index: its own sort key is in force, and so is the
        // table's partition key, which that index is keyed by too.
        Row {
            case: "a local index query filtering on the table's sort key",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": LOCAL,
                            "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "sk = :n",
                            "ExpressionAttributeValues": values(&[":p", ":n"])}),
            refused: None,
        },
        Row {
            case: "a local index query filtering on its own sort key",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": LOCAL,
                            "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "lsk = :l",
                            "ExpressionAttributeValues": values(&[":p", ":l"])}),
            refused: Some(not_a_filter_attribute("Filter Expression", "lsk")),
        },
        Row {
            case: "a local index query filtering on the key it shares with the table",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": LOCAL,
                            "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "pk = :p",
                            "ExpressionAttributeValues": values(&[":p"])}),
            refused: Some(not_a_filter_attribute("Filter Expression", "pk")),
        },
        // The LEGACY generation states the same filter, so it is held to the same rule and
        // told about the map it sent.
        Row {
            case: "a legacy index query filtering on the table's sort key",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL,
                            "KeyConditions": {"gpk": {"ComparisonOperator": "EQ",
                                                      "AttributeValueList": [{"S": "g"}]}},
                            "QueryFilter": {"sk": {"ComparisonOperator": "EQ",
                                                   "AttributeValueList": [{"N": "2"}]}}}),
            refused: None,
        },
        Row {
            case: "a legacy index query filtering on the index's own range key",
            operation: "Query",
            request: json!({"TableName": TABLE, "IndexName": GLOBAL,
                            "KeyConditions": {"gpk": {"ComparisonOperator": "EQ",
                                                      "AttributeValueList": [{"S": "g"}]}},
                            "QueryFilter": {"gsk": {"ComparisonOperator": "EQ",
                                                    "AttributeValueList": [{"N": "1"}]}}}),
            refused: Some(not_a_filter_attribute("QueryFilter", "gsk")),
        },
    ]
}

#[tokio::test]
async fn a_filter_is_held_to_the_key_schema_the_read_addresses() {
    let router = emulator();
    seeded(&router).await;

    for row in rows() {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        match row.refused.as_deref() {
            Some(stated) => assert_eq!(
                (status, error_code(&body).as_str(), message(&body).as_str()),
                (400, "ValidationException", stated),
                "case {:?}: expected the refusal {stated:?}, got {status} with {body}",
                row.case
            ),
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: a filter over an attribute no key schema of this read \
                     covers is served, got {status} with {body}",
                    row.case
                );
                // The page is the NARROWED one: the filter was evaluated over every entry
                // the read walked, and only the item it selects comes back.
                assert_eq!(
                    (body["Count"].clone(), body["ScannedCount"].clone()),
                    (json!(1), json!(ITEMS)),
                    "case {:?}: expected the one narrowed item of {ITEMS} walked, got \
                     {body}",
                    row.case
                );
                assert_eq!(
                    body["Items"][0]["sk"],
                    json!({"N": NARROWED.to_string()}),
                    "case {:?}: expected the item the filter selects, got {body}",
                    row.case
                );
            }
        }
    }
}

#[tokio::test]
async fn a_scan_states_no_key_condition_so_it_may_filter_on_a_key_attribute() {
    let router = emulator();
    seeded(&router).await;

    // A `Scan` narrows what it walked rather than what a key condition selected, so no key
    // schema is in force for its filter — whether it walks the table or an index.
    for (case, request) in [
        (
            "a scan of the table filtering on its sort key",
            json!({"TableName": TABLE, "FilterExpression": "sk = :n",
                   "ExpressionAttributeValues": values(&[":n"])}),
        ),
        (
            "a scan of the global index filtering on the table's sort key",
            json!({"TableName": TABLE, "IndexName": GLOBAL,
                   "FilterExpression": "sk = :n",
                   "ExpressionAttributeValues": values(&[":n"])}),
        ),
        (
            "a scan of the global index filtering on that index's own range key",
            json!({"TableName": TABLE, "IndexName": GLOBAL,
                   "FilterExpression": "gsk = :gs",
                   "ExpressionAttributeValues": values(&[":gs"])}),
        ),
    ] {
        let (status, body) = call(&router, "Scan", request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a scan's filter is held to no key schema, got {status} with \
             {body}"
        );
    }
}
