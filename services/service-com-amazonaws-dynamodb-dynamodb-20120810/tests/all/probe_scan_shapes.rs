// SPDX-License-Identifier: CC0-1.0

//! A valid filter is served whatever ELSE a `Scan` asks for, driven end to end.
//!
//! The rule pinned here: whether a filter is served is decided by the filter alone, so a
//! request carrying one is answered the same way whichever other members it states — the
//! index it reads, the `Select` it asks for, a projection, a page limit, a consistent read,
//! a parallel segment, a capacity report — and the filter's own attributes need not be
//! projected into the index it reads.
//!
//! The rows span those members over one filter, so a refusal here is a correct query a
//! client cannot run.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, message, ok};

/// The table every row reads, and the global index one row reads it through.
const TABLE: &str = "ScanFilterRequestShapes";
const INDEX: &str = "by-s";

#[tokio::test]
async fn a_valid_filter_is_served_whatever_else_the_scan_states() {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                     {"AttributeName": "s", "AttributeType": "S"}],
            "GlobalSecondaryIndexes": [{
                "IndexName": INDEX,
                "KeySchema": [{"AttributeName": "s", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the scan-shape table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "k1"}, "s": {"S": "beta"},
                                            "peer": {"S": "beta"}, "n": {"N": "3"}}}),
    )
    .await;

    let rows: Vec<(&str, Value)> = vec![
        (
            "nothing but the filter",
            json!({"FilterExpression": "contains(s, peer)"}),
        ),
        (
            "an index scan",
            json!({"IndexName": INDEX, "FilterExpression": "contains(s, peer)"}),
        ),
        (
            "an index scan filtering on a name the index does not key by",
            json!({"IndexName": INDEX, "FilterExpression": "attribute_exists(peer)"}),
        ),
        (
            "a counted scan",
            json!({"Select": "COUNT", "FilterExpression": "contains(s, peer)"}),
        ),
        (
            "a projected scan",
            json!({"ProjectionExpression": "pk, s", "FilterExpression": "contains(s, peer)"}),
        ),
        (
            "a page-limited scan",
            json!({"Limit": 1, "FilterExpression": "contains(s, peer)"}),
        ),
        (
            "a consistent scan",
            json!({"ConsistentRead": true, "FilterExpression": "contains(s, peer)"}),
        ),
        (
            "one segment of a parallel scan",
            json!({"Segment": 0, "TotalSegments": 1, "FilterExpression": "contains(s, peer)"}),
        ),
        (
            "a capacity-reporting scan",
            json!({"ReturnConsumedCapacity": "TOTAL",
                   "FilterExpression": "contains(s, peer)"}),
        ),
    ];

    for (case, members) in rows {
        let mut request = members.clone();
        request["TableName"] = json!(TABLE);
        let (status, answered) = call(&router, "Scan", request).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: expected a served scan, got {status} with {}",
            message(&answered)
        );
        assert_eq!(
            (answered["Count"].clone(), answered["ScannedCount"].clone()),
            (json!(1), json!(1)),
            "case {case:?}: expected the one matching item counted, got {answered}"
        );
    }
}
