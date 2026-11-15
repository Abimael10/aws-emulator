// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch counting series this service publishes for what a request's OUTCOME was.
//!
//! Two rules are under test, each over its own family and its own dimension set:
//!
//! * the items a read HANDS BACK are published as `AWS/DynamoDB` `ReturnedItemCount` under the
//!   dimension PAIR `{TableName, Operation}`, one observation per served read, carrying the
//!   number of items that read returned — so a `Sum` over a window is the items the table
//!   handed back through that API and the `SampleCount` is how many such reads it served;
//! * every write REFUSED because the condition it stated was not met is published as
//!   `AWS/DynamoDB` `ConditionalCheckFailedRequests` under `{TableName}`, one observation of
//!   one request, whichever surface the refused write came in through.
//!
//! Each family's dimension set is its whole identity: nothing of `ReturnedItemCount` is
//! published under `{TableName}` alone, nothing of `ConditionalCheckFailedRequests` under the
//! pair, and an operation whose reads were never made has no series at all.
//!
//! The catalogs span what the rules generalise over — which API the read was made with, how
//! many items it returned (including none), whether a filter or a page limit cut the page
//! down, how many reads were made, which surface refused the write, and how many writes were
//! refused — rather than one operation against one table.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::emulator::{
    call, describe_when_active, emulator_with_cloudwatch, error_code, ok, ok_cloudwatch,
};

/// The namespace Amazon DynamoDB publishes a table's own metrics under.
const NAMESPACE: &str = "AWS/DynamoDB";

/// The metric that counts the items a read handed back.
const RETURNED_ITEM_COUNT: &str = "ReturnedItemCount";

/// The metric that counts the writes a condition turned down.
const CONDITIONAL_CHECK_FAILED: &str = "ConditionalCheckFailedRequests";

/// The dimension that names the table a series belongs to.
const TABLE_NAME: &str = "TableName";

/// The dimension that names the API the requests of a series were made with.
const OPERATION: &str = "Operation";

/// The unit a count is published in.
const COUNT_UNIT: &str = "Count";

/// The partition the seeded items share, which the queries address.
const PARTITION: &str = "p";

/// A partition no seeded item carries, so a read of it matches nothing.
const EMPTY_PARTITION: &str = "absent";

/// The `tag` value exactly one seeded item carries, which a filter keeps.
const KEPT_TAG: &str = "keep";

/// How many items the subject table is seeded with: three in [`PARTITION`] and one outside it.
const SEEDED_ITEMS: f64 = 4.0;

/// The largest difference two counts may show and still be the same count: both sides are sums
/// of whole items, so anything above rounding is a real divergence.
const TOLERANCE: f64 = 1e-9;

/// One row of the read catalog: an API, the reads made with it, and what they returned.
struct ReadRow {
    case: &'static str,
    /// The API served, which is the `Operation` dimension value its observations are booked
    /// under.
    operation: &'static str,
    /// The request bodies sent, one per call.
    bodies: fn(&str) -> Vec<Value>,
    /// The items the reads of this row return in total, which is what their series gains.
    returned: f64,
}

/// The catalog: the three APIs AWS documents this family over, over reads that return three
/// items, one item, no item and a page cut short, and over one and two calls.
///
/// The operations are the ones the family is documented for — "the number of items returned
/// by `Query`, `Scan`, or `ExecuteStatement` operations" — so the statement surface is here
/// beside the two item-API reads.
const READ_ROWS: &[ReadRow] = &[
    ReadRow {
        case: "a Query returning the three items of one partition",
        operation: "Query",
        bodies: |table| vec![query(table, PARTITION, None)],
        returned: 3.0,
    },
    ReadRow {
        case: "two Querys of the same partition",
        operation: "Query",
        bodies: |table| {
            (0..2)
                .map(|_| query(table, PARTITION, None))
                .collect()
        },
        returned: 6.0,
    },
    ReadRow {
        case: "a Query whose filter keeps one of the three items it evaluated",
        operation: "Query",
        bodies: |table| vec![query(table, PARTITION, Some(KEPT_TAG))],
        returned: 1.0,
    },
    ReadRow {
        case: "a Query of a partition that holds nothing",
        operation: "Query",
        bodies: |table| vec![query(table, EMPTY_PARTITION, None)],
        returned: 0.0,
    },
    ReadRow {
        case: "a Scan of the whole table",
        operation: "Scan",
        bodies: |table| vec![json!({"TableName": table})],
        returned: SEEDED_ITEMS,
    },
    ReadRow {
        case: "a Scan stopped at a page limit of two",
        operation: "Scan",
        bodies: |table| vec![json!({"TableName": table, "Limit": 2})],
        returned: 2.0,
    },
    ReadRow {
        case: "a Scan whose filter keeps one of the four items it evaluated",
        operation: "Scan",
        bodies: |table| {
            vec![json!({
                "TableName": table,
                "FilterExpression": "tag = :tag",
                "ExpressionAttributeValues": {":tag": {"S": KEPT_TAG}},
            })]
        },
        returned: 1.0,
    },
    ReadRow {
        case: "a statement SELECT over one partition",
        operation: "ExecuteStatement",
        bodies: |table| {
            vec![json!({
                "Statement": format!("SELECT * FROM {table} WHERE pk = '{PARTITION}'"),
            })]
        },
        returned: 3.0,
    },
    ReadRow {
        case: "two statement SELECTs, one of them matching nothing",
        operation: "ExecuteStatement",
        bodies: |table| {
            vec![
                json!({
                    "Statement": format!("SELECT * FROM {table} WHERE pk = '{PARTITION}'"),
                }),
                json!({
                    "Statement": format!(
                        "SELECT * FROM {table} WHERE pk = '{EMPTY_PARTITION}'"
                    ),
                }),
            ]
        },
        returned: 3.0,
    },
];

#[tokio::test]
async fn every_read_counts_the_items_it_returned_under_its_own_operation() {
    let router = emulator_with_cloudwatch();
    let table = "returned";
    create_table(&router, table).await;
    seed_items(&router, table).await;

    for row in READ_ROWS {
        let case = row.case;
        // Measured as the DELTA this row's reads add, so each row stands on its own even
        // where two of them read through the same API.
        let before = series(&router, RETURNED_ITEM_COUNT, pair(table, row.operation)).await;
        let mut calls = 0.0_f64;
        for body in (row.bodies)(table) {
            calls += 1.0;
            let (status, answered) = call(&router, row.operation, body.clone()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {} {body} to answer 200, got {status} {answered:?}",
                row.operation
            );
        }
        let after = series(&router, RETURNED_ITEM_COUNT, pair(table, row.operation)).await;

        let counted = after.sum - before.sum;
        assert!(
            (counted - row.returned).abs() < TOLERANCE,
            "case {case:?}: expected the {RETURNED_ITEM_COUNT} series of {table:?} under \
             Operation={} to gain {} items, got {counted} (from {before:?} to {after:?})",
            row.operation,
            row.returned
        );
        let samples = after.sample_count - before.sample_count;
        assert!(
            (samples - calls).abs() < TOLERANCE,
            "case {case:?}: expected the {RETURNED_ITEM_COUNT} series of {table:?} under \
             Operation={} to gain {calls} samples — one per read served — got {samples}",
            row.operation
        );
        for unit in &after.units {
            assert_eq!(
                unit, COUNT_UNIT,
                "case {case:?}: expected a {RETURNED_ITEM_COUNT} datapoint in {COUNT_UNIT}, \
                 got {unit:?}"
            );
        }
        // The pair is the whole identity of the family: nothing of it is published under the
        // table dimension alone.
        let table_only = series(
            &router,
            RETURNED_ITEM_COUNT,
            json!([{"Name": TABLE_NAME, "Value": table}]),
        )
        .await;
        assert!(
            table_only.datapoints == 0,
            "case {case:?}: expected no {RETURNED_ITEM_COUNT} series under {TABLE_NAME} alone, \
             got {table_only:?}"
        );
        // And CloudWatch LISTS the series the reads published, under the pair they were
        // published with.
        let listed = listed_dimension_sets(&router, RETURNED_ITEM_COUNT, table).await;
        let expected = vec![
            (OPERATION.to_string(), row.operation.to_string()),
            (TABLE_NAME.to_string(), table.to_string()),
        ];
        assert!(
            listed.contains(&expected),
            "case {case:?}: expected ListMetrics({NAMESPACE}, {RETURNED_ITEM_COUNT}, \
             {TABLE_NAME}={table:?}) to list {expected:?}, got {listed:?}"
        );
    }
}

/// The reads this family is NOT published for publish nothing: AWS documents
/// `ReturnedItemCount` over `Query`, `Scan` and `ExecuteStatement`, so the key-addressed
/// reads — which return the one item a key holds rather than a page they evaluated — have no
/// series of it, and neither has an operation nobody called.
#[tokio::test]
async fn only_the_page_reads_of_the_family_publish_returned_item_count() {
    let router = emulator_with_cloudwatch();
    let table = "unpublished";
    let case = "the reads outside the family";
    create_table(&router, table).await;
    seed_items(&router, table).await;

    ok(
        &router,
        "GetItem",
        json!({"TableName": table, "Key": key(PARTITION, "a")}),
    )
    .await;
    ok(
        &router,
        "BatchGetItem",
        json!({"RequestItems": {table: {"Keys": [key(PARTITION, "a")]}}}),
    )
    .await;
    ok(
        &router,
        "TransactGetItems",
        json!({
            "TransactItems": [{"Get": {"TableName": table, "Key": key(PARTITION, "a")}}],
        }),
    )
    .await;
    for operation in ["GetItem", "BatchGetItem", "TransactGetItems", "Query", "Scan"] {
        let published = series(&router, RETURNED_ITEM_COUNT, pair(table, operation)).await;
        assert!(
            published.datapoints == 0,
            "case {case:?}: expected no {RETURNED_ITEM_COUNT} series under Operation={operation} \
             on {table:?}, got {published:?}"
        );
    }
}

/// One row of the refusal catalog: a surface that refuses a conditional write, and how many
/// writes this row has it refuse.
struct RefusalRow {
    case: &'static str,
    /// The operation the refused writes are made with.
    operation: &'static str,
    /// How many writes of this row are refused, each of which is one failed attempt.
    refusals: usize,
    /// How this surface reports the refusal.
    reported: Reported,
    /// The request the table refuses: the write guarded by a condition the stored item does
    /// not satisfy.
    refused: fn(&str) -> Value,
}

/// How a surface reports the refusal of a write whose condition was not met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    /// A modelled `ConditionalCheckFailedException`, HTTP 400.
    Modelled,
    /// A `TransactionCanceledException` whose reasons name the element that failed.
    Cancelled,
    /// A served batch whose SLOT carries the failure code.
    Slot,
}

/// The catalog: every surface a conditional write can be refused on, over one, two and three
/// refusals.
const REFUSAL_ROWS: &[RefusalRow] = &[
    RefusalRow {
        case: "three PutItems whose condition the stored item does not satisfy",
        operation: "PutItem",
        refusals: 3,
        reported: Reported::Modelled,
        refused: |table| {
            json!({
                "TableName": table,
                "Item": seeded_item(PARTITION, "a", KEPT_TAG),
                "ConditionExpression": "attribute_not_exists(pk)",
            })
        },
    },
    RefusalRow {
        case: "two refused UpdateItems",
        operation: "UpdateItem",
        refusals: 2,
        reported: Reported::Modelled,
        refused: |table| {
            json!({
                "TableName": table,
                "Key": key(PARTITION, "a"),
                "UpdateExpression": "SET tag = :tag",
                "ConditionExpression": "tag = :other",
                "ExpressionAttributeValues": {
                    ":tag": {"S": KEPT_TAG},
                    ":other": {"S": "no-such-tag"},
                },
            })
        },
    },
    RefusalRow {
        case: "one refused DeleteItem",
        operation: "DeleteItem",
        refusals: 1,
        reported: Reported::Modelled,
        refused: |table| {
            json!({
                "TableName": table,
                "Key": key(PARTITION, "a"),
                "ConditionExpression": "attribute_not_exists(pk)",
            })
        },
    },
    RefusalRow {
        case: "two refused statement-surface UPDATEs",
        operation: "ExecuteStatement",
        refusals: 2,
        reported: Reported::Modelled,
        refused: |table| json!({"Statement": guarded_statement(table)}),
    },
    RefusalRow {
        case: "a refused slot of a batched statement",
        operation: "BatchExecuteStatement",
        refusals: 1,
        reported: Reported::Slot,
        refused: |table| json!({"Statements": [{"Statement": guarded_statement(table)}]}),
    },
    RefusalRow {
        case: "a transaction cancelled by one element's unmet condition",
        operation: "TransactWriteItems",
        refusals: 2,
        reported: Reported::Cancelled,
        refused: |table| {
            json!({
                "TransactItems": [{
                    "Put": {
                        "TableName": table,
                        "Item": seeded_item(PARTITION, "a", KEPT_TAG),
                        "ConditionExpression": "attribute_not_exists(pk)",
                    },
                }],
            })
        },
    },
    RefusalRow {
        case: "a statement transaction cancelled by an unmet predicate",
        operation: "ExecuteTransaction",
        refusals: 1,
        reported: Reported::Cancelled,
        refused: |table| {
            json!({"TransactStatements": [{"Statement": guarded_statement(table)}]})
        },
    },
];

#[tokio::test]
async fn every_refused_conditional_write_counts_one_failed_attempt() {
    let router = emulator_with_cloudwatch();
    let table = "refusals";
    create_table(&router, table).await;
    seed_items(&router, table).await;

    for row in REFUSAL_ROWS {
        let case = row.case;
        let dimensions = json!([{"Name": TABLE_NAME, "Value": table}]);
        let before = series(&router, CONDITIONAL_CHECK_FAILED, dimensions.clone()).await;
        for attempt in 0..row.refusals {
            let body = (row.refused)(table);
            let (status, answered) = call(&router, row.operation, body.clone()).await;
            assert_refused(case, attempt, row, status, &answered);
        }
        let after = series(&router, CONDITIONAL_CHECK_FAILED, dimensions).await;

        let counted = after.sum - before.sum;
        let expected = row.refusals as f64;
        assert!(
            (counted - expected).abs() < TOLERANCE,
            "case {case:?}: expected the {CONDITIONAL_CHECK_FAILED} series of {table:?} to gain \
             {expected} from the {} refused {} writes, got {counted} (from {before:?} to \
             {after:?})",
            row.refusals,
            row.operation
        );
        let samples = after.sample_count - before.sample_count;
        assert!(
            (samples - expected).abs() < TOLERANCE,
            "case {case:?}: expected {expected} samples — one per refused write — got {samples}"
        );
        for unit in &after.units {
            assert_eq!(
                unit, COUNT_UNIT,
                "case {case:?}: expected a {CONDITIONAL_CHECK_FAILED} datapoint in \
                 {COUNT_UNIT}, got {unit:?}"
            );
        }
        // The table dimension is the whole identity of this family: nothing of it is
        // published beside an operation.
        let under_operation =
            series(&router, CONDITIONAL_CHECK_FAILED, pair(table, row.operation)).await;
        assert!(
            under_operation.datapoints == 0,
            "case {case:?}: expected no {CONDITIONAL_CHECK_FAILED} series under \
             Operation={}, got {under_operation:?}",
            row.operation
        );
        // And CloudWatch lists the series under the table dimension it was published with.
        let listed = listed_dimension_sets(&router, CONDITIONAL_CHECK_FAILED, table).await;
        let expected_set = vec![(TABLE_NAME.to_string(), table.to_string())];
        assert!(
            listed.contains(&expected_set),
            "case {case:?}: expected ListMetrics({NAMESPACE}, {CONDITIONAL_CHECK_FAILED}, \
             {TABLE_NAME}={table:?}) to list {expected_set:?}, got {listed:?}"
        );
    }
}

/// A write whose condition the stored item DOES satisfy is no failed attempt, and neither is
/// an unguarded write: a table whose writers all win a race has no series of this family at
/// all.
#[tokio::test]
async fn a_write_that_was_not_refused_counts_nothing() {
    let router = emulator_with_cloudwatch();
    let table = "satisfied";
    let case = "writes no condition turned down";
    create_table(&router, table).await;
    seed_items(&router, table).await;

    ok(
        &router,
        "PutItem",
        json!({
            "TableName": table,
            "Item": seeded_item(PARTITION, "a", KEPT_TAG),
            "ConditionExpression": "attribute_exists(pk)",
        }),
    )
    .await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": table, "Item": seeded_item(PARTITION, "d", KEPT_TAG)}),
    )
    .await;

    let published = series(
        &router,
        CONDITIONAL_CHECK_FAILED,
        json!([{"Name": TABLE_NAME, "Value": table}]),
    )
    .await;
    assert!(
        published.datapoints == 0,
        "case {case:?}: expected no {CONDITIONAL_CHECK_FAILED} series on {table:?}, got \
         {published:?}"
    );
}

/// Every refused write of a row is reported the way its surface reports an unmet condition.
fn assert_refused(case: &str, attempt: usize, row: &RefusalRow, status: u16, answered: &Value) {
    match row.reported {
        Reported::Modelled => {
            assert_eq!(
                (status, error_code(answered)),
                (400, "ConditionalCheckFailedException".to_string()),
                "case {case:?}, attempt {attempt}: expected the modelled refusal, got {status} \
                 {answered:?}"
            );
        }
        Reported::Cancelled => {
            assert_eq!(
                (status, error_code(answered)),
                (400, "TransactionCanceledException".to_string()),
                "case {case:?}, attempt {attempt}: expected the cancellation, got {status} \
                 {answered:?}"
            );
            let reasons = answered["CancellationReasons"].to_string();
            assert!(
                reasons.contains("ConditionalCheckFailed"),
                "case {case:?}, attempt {attempt}: expected a reason naming the unmet \
                 condition, got {reasons}"
            );
        }
        Reported::Slot => {
            assert_eq!(
                status, 200,
                "case {case:?}, attempt {attempt}: a batch serves its slots, got {status} \
                 {answered:?}"
            );
            let slots = answered["Responses"].to_string();
            assert!(
                slots.contains("ConditionalCheckFailed"),
                "case {case:?}, attempt {attempt}: expected the slot to carry the failure, got \
                 {slots}"
            );
        }
    }
}

/// One read of a `{pk, sk}` table: the partition the condition pins, and the `tag` value a
/// filter keeps where the row states one.
fn query(table: &str, partition: &str, kept_tag: Option<&str>) -> Value {
    let mut request = json!({
        "TableName": table,
        "KeyConditionExpression": "pk = :pk",
        "ExpressionAttributeValues": {":pk": {"S": partition}},
    });
    if let Some(tag) = kept_tag {
        request["FilterExpression"] = json!("tag = :tag");
        request["ExpressionAttributeValues"][":tag"] = json!({"S": tag});
    }
    request
}

/// The statement surfaces' guarded write: the `WHERE` clause pins the seeded key and states a
/// `tag` no stored item carries, so the write is always refused.
fn guarded_statement(table: &str) -> String {
    format!(
        "UPDATE \"{table}\" SET tag = '{KEPT_TAG}' WHERE pk = '{PARTITION}' AND sk = 'a' AND \
         tag = 'no-such-tag'"
    )
}

/// The key of one seeded item.
fn key(partition: &str, sort: &str) -> Value {
    json!({"pk": {"S": partition}, "sk": {"S": sort}})
}

/// One seeded item: its key and the `tag` attribute the filters read.
fn seeded_item(partition: &str, sort: &str, tag: &str) -> Value {
    json!({"pk": {"S": partition}, "sk": {"S": sort}, "tag": {"S": tag}})
}

/// Create one on-demand table keyed by a partition and a sort attribute, ready to serve.
async fn create_table(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
            ],
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    // A table is served once it is ACTIVE: the waiter loop a client runs after a create.
    describe_when_active(router, name, name).await;
}

/// Seed [`SEEDED_ITEMS`] items: three in [`PARTITION`], exactly one of them carrying the tag a
/// filter keeps, and one outside that partition so a scan sees more than a query does.
async fn seed_items(router: &MultiServiceRouter, table: &str) {
    for (partition, sort, tag) in [
        (PARTITION, "a", KEPT_TAG),
        (PARTITION, "b", "drop"),
        (PARTITION, "c", "drop"),
        ("q", "z", "drop"),
    ] {
        ok(
            router,
            "PutItem",
            json!({"TableName": table, "Item": seeded_item(partition, sort, tag)}),
        )
        .await;
    }
}

/// The dimension set of a per-operation series, as `GetMetricStatistics` takes it.
fn pair(table: &str, operation: &str) -> Value {
    json!([
        {"Name": TABLE_NAME, "Value": table},
        {"Name": OPERATION, "Value": operation},
    ])
}

/// What one series holds over a window wide enough for everything a test published.
#[derive(Debug)]
struct Reading {
    /// How many datapoints the window reported.
    datapoints: usize,
    /// The sum of their `Sum` statistics: what the requests counted.
    sum: f64,
    /// The sum of their `SampleCount` statistics: how many requests contributed.
    sample_count: f64,
    /// The unit each datapoint carries.
    units: Vec<String>,
}

/// Read one series of this namespace back through CloudWatch.
async fn series(router: &MultiServiceRouter, metric: &str, dimensions: Value) -> Reading {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    let answered = ok_cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - 3600.0,
            "EndTime": now + 3600.0,
            "Period": 60,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    let points: Vec<Value> = answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Reading {
        datapoints: points.len(),
        sum: points.iter().filter_map(|point| point["Sum"].as_f64()).sum(),
        sample_count: points
            .iter()
            .filter_map(|point| point["SampleCount"].as_f64())
            .sum(),
        units: points
            .iter()
            .filter_map(|point| point["Unit"].as_str().map(str::to_string))
            .collect(),
    }
}

/// Every dimension set CloudWatch lists for `metric` under a `TableName` filter naming
/// `table`, each as its `(name, value)` pairs in name order.
async fn listed_dimension_sets(
    router: &MultiServiceRouter,
    metric: &str,
    table: &str,
) -> Vec<Vec<(String, String)>> {
    let answered = ok_cloudwatch(
        router,
        "ListMetrics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": TABLE_NAME, "Value": table}],
        }),
    )
    .await;
    answered["Metrics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|listed| {
            let mut pairs: Vec<(String, String)> = listed["Dimensions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            pairs.sort();
            pairs
        })
        .collect()
}
