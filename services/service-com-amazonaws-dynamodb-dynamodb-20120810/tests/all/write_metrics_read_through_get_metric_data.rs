// SPDX-License-Identifier: CC0-1.0

//! What a client building a capacity dashboard reads out of `GetMetricData` for a table it
//! has written to.
//!
//! The rule under test: a write of an item costs one write capacity unit per kilobyte of
//! ITEM SIZE, rounded up and never fewer than one ("One write capacity unit covers a write of
//! up to 1 KB" — DynamoDB Developer Guide, "Read/write capacity mode"), a transactional write
//! of the same item costs exactly twice that ("Transactional write requests require two write
//! capacity units"), those units land in the `AWS/DynamoDB` `ConsumedWriteCapacityUnits`
//! series of the table under `{TableName}`, and each served request books one
//! `SuccessfulRequestLatency` observation under the dimension PAIR `{TableName, Operation}`
//! for the API it was made with. Both families live in the ONE metric store, so a
//! `GetMetricData` `MetricStat` query naming the series folds out exactly those figures —
//! whether or not the write asked for its `ConsumedCapacity` in the response.
//!
//! The suites beside this one read the same families through `GetMetricStatistics`
//! (`request_latency_metrics.rs`, `transactional_consumed_capacity_metrics.rs`); this one
//! reads them through the query surface a dashboard, an alarm preview and an autoscaling
//! trigger use, and pins the ABSOLUTE unit figure the rounding rule gives rather than only
//! its agreement with the response.
//!
//! The catalog spans what the rule generalises over: the item size, around and across the
//! kilobyte unit boundary (just under it, exactly on it, one byte past it, several units
//! wide, and an item far below one unit), how many items are written, whether the sizes of
//! one row differ, and whether the request asked for its `ConsumedCapacity` at all.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::emulator::{call, describe_when_active, emulator_with_cloudwatch, ok, ok_cloudwatch};

/// The namespace Amazon DynamoDB publishes a table's own metrics under.
const NAMESPACE: &str = "AWS/DynamoDB";

/// The metric that counts the write capacity units a table consumed.
const CONSUMED_WRITE: &str = "ConsumedWriteCapacityUnits";

/// The metric that reports how long a served request took.
const LATENCY: &str = "SuccessfulRequestLatency";

/// The dimension that names the table a series belongs to.
const TABLE_NAME: &str = "TableName";

/// The dimension that names the API a latency series belongs to, beside the table.
const OPERATION: &str = "Operation";

/// The unit an elapsed request time is reported in.
const MILLISECONDS: &str = "Milliseconds";

/// The attribute names each written item carries, whose byte lengths are part of the item's
/// size: "the stored size of one item is every attribute's name plus its value".
const KEY_ATTRIBUTE: &str = "pk";
const PAYLOAD_ATTRIBUTE: &str = "payload";

/// How wide a window the reads fold, in seconds, and how far either side of now they reach:
/// every datapoint this suite publishes falls inside, and the figures asserted are the sum
/// over the windows the response carries, so where the minute boundary falls does not matter.
const PERIOD: i64 = 60;
const REACH: f64 = 1_800.0;

/// The largest difference two capacity figures may show and still be the same figure: both
/// sides are sums of whole units, so anything above rounding is a real divergence.
const TOLERANCE: f64 = 1e-9;

/// One row of the catalog: the items written, and what the rule says they cost.
struct SizeRow {
    case: &'static str,
    /// The payload byte counts of the items written, one item per entry. An entry of `0` is
    /// an item of its key alone.
    payloads: &'static [usize],
    /// The item sizes those payloads come to, in bytes — asserted against the rule that sizes
    /// an item, so a row states the size it is about rather than implying it.
    sizes: &'static [i64],
    /// The write capacity units a plain write of those items costs: one unit per kilobyte of
    /// each item's size, rounded up, never fewer than one.
    plain_units: f64,
    /// Whether the writes ask for `ConsumedCapacity` in the response — the series is the
    /// table's traffic either way.
    reports_capacity: bool,
}

/// The catalog: the kilobyte boundary from either side, an item several units wide, an item
/// far below one unit, and the mixed-size row whose two item sizes are charged differently.
const SIZE_ROWS: &[SizeRow] = &[
    SizeRow {
        case: "one item a byte under the unit boundary",
        payloads: &[1_012],
        sizes: &[1_023],
        plain_units: 1.0,
        reports_capacity: true,
    },
    SizeRow {
        case: "one item exactly on the unit boundary",
        payloads: &[1_013],
        sizes: &[1_024],
        plain_units: 1.0,
        reports_capacity: true,
    },
    SizeRow {
        case: "one item a byte past the unit boundary",
        payloads: &[1_014],
        sizes: &[1_025],
        plain_units: 2.0,
        reports_capacity: true,
    },
    SizeRow {
        case: "three items on the boundary and three a byte past it",
        payloads: &[1_013, 1_013, 1_013, 1_014, 1_014, 1_014],
        sizes: &[1_024, 1_024, 1_024, 1_025, 1_025, 1_025],
        // Three single-unit writes and three two-unit ones.
        plain_units: 9.0,
        reports_capacity: true,
    },
    SizeRow {
        case: "two items several units wide",
        payloads: &[2_037, 3_061],
        sizes: &[2_048, 3_072],
        // Exactly two units and exactly three.
        plain_units: 5.0,
        reports_capacity: true,
    },
    SizeRow {
        case: "two items far below one unit, written without asking for the capacity",
        payloads: &[0, 20],
        sizes: &[4, 31],
        // A write is never charged less than one unit, however small the item.
        plain_units: 2.0,
        reports_capacity: false,
    },
];

/// The plain and the transactional write of the SAME items, read back through
/// `GetMetricData`: the table written through the item API carries the units the rounding rule
/// gives, the twin written through single-put transactions carries exactly twice them, and
/// each table's latency samples sit under the one operation it was written with.
#[tokio::test]
async fn get_metric_data_reports_the_units_each_write_surface_charged() {
    for (index, row) in SIZE_ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator_with_cloudwatch();
        let plain = format!("item-api-{index}");
        let transactional = format!("transactional-{index}");
        // Both creates are made before either wait, so the twin tables settle together.
        for table in [&plain, &transactional] {
            create_table(&router, table).await;
        }
        for table in [&plain, &transactional] {
            describe_when_active(&router, case, table).await;
        }

        // The sizes the row is about are the sizes the items really have. This guards the
        // catalog against a typo rather than the emulator against a defect — the behavioural
        // cover is the unit figure read out of the published series below.
        assert_eq!(
            row.payloads.len(),
            row.sizes.len(),
            "case {case:?}: every payload of a row states the item size it comes to"
        );
        for (position, (payload, size)) in row.payloads.iter().zip(row.sizes).enumerate() {
            let measured = item_size(&key(position), *payload);
            assert_eq!(
                measured, *size,
                "case {case:?}: expected the item of {payload} payload bytes to be {size} bytes \
                 of attribute names and values, got {measured}"
            );
        }

        let mut plain_reported = 0.0_f64;
        let mut transactional_reported = 0.0_f64;
        for (position, payload) in row.payloads.iter().enumerate() {
            let written = item(&key(position), *payload);
            let mut put = json!({"TableName": plain, "Item": written.clone()});
            let mut transact = json!({
                "TransactItems": [{"Put": {"TableName": transactional, "Item": written}}],
            });
            if row.reports_capacity {
                put["ReturnConsumedCapacity"] = json!("TOTAL");
                transact["ReturnConsumedCapacity"] = json!("TOTAL");
            }
            plain_reported += reported_units(&answered(&router, case, "PutItem", put).await);
            transactional_reported += reported_units(
                &answered(&router, case, "TransactWriteItems", transact).await,
            );
        }

        // What the item API spent, as the one-unit-per-kilobyte rounding gives it.
        let published = series_sum(&router, &plain, CONSUMED_WRITE, None).await;
        assert!(
            (published - row.plain_units).abs() < TOLERANCE,
            "case {case:?}: expected the {CONSUMED_WRITE} series of {plain:?} to carry the \
             {} units the item sizes {:?} cost, got {published}",
            row.plain_units,
            row.sizes
        );
        // And the transactional twin, charged twice over for the same items.
        let doubled = series_sum(&router, &transactional, CONSUMED_WRITE, None).await;
        assert!(
            (doubled - 2.0 * row.plain_units).abs() < TOLERANCE,
            "case {case:?}: expected the {CONSUMED_WRITE} series of {transactional:?} to carry \
             twice the {} units of {plain:?}, got {doubled}",
            row.plain_units
        );

        // A request that asked for its ConsumedCapacity was told the same figures; one that
        // did not was told nothing, and the series carries the traffic all the same.
        let expected_reported = if row.reports_capacity {
            (row.plain_units, 2.0 * row.plain_units)
        } else {
            (0.0, 0.0)
        };
        assert!(
            (plain_reported - expected_reported.0).abs() < TOLERANCE
                && (transactional_reported - expected_reported.1).abs() < TOLERANCE,
            "case {case:?}: expected the responses to report {expected_reported:?} units, got \
             ({plain_reported}, {transactional_reported})"
        );

        // Each table's requests are booked under the operation they were made with, in
        // milliseconds, and the table written with the other operation has no series for it.
        let calls = row.payloads.len() as f64;
        for (table, served, unserved) in [
            (&plain, "PutItem", "TransactWriteItems"),
            (&transactional, "TransactWriteItems", "PutItem"),
        ] {
            let samples = series_sum(&router, table, LATENCY, Some(served)).await;
            assert!(
                (samples - calls).abs() < TOLERANCE,
                "case {case:?}: expected {table:?} to book {calls} {served} latency samples, got \
                 {samples}"
            );
            let results = latency_results(&router, table, served).await;
            for result in &results {
                let label = result["Label"].as_str().unwrap_or_default();
                assert_eq!(
                    label, LATENCY,
                    "case {case:?}: expected a {served} result labelled with the metric, got \
                     {result}"
                );
            }
            let elapsed: f64 = values(&results).iter().sum();
            assert!(
                elapsed > 0.0 && elapsed.is_finite(),
                "case {case:?}: expected the {served} latency of {table:?} to be a positive \
                 elapsed time, got {elapsed} over {results:?}"
            );
            let absent = series_sum(&router, table, LATENCY, Some(unserved)).await;
            assert!(
                absent.abs() < TOLERANCE,
                "case {case:?}: expected no {unserved} latency samples on {table:?}, got {absent}"
            );
        }
    }
}

/// The identity of each series is the whole dimension set a `MetricStat` names: the
/// consumption family is published under the table ALONE and the latency family under the
/// PAIR, so a query that names the other set answers no datapoints at all — which is what an
/// alarm sitting in `INSUFFICIENT_DATA` is reading.
#[tokio::test]
async fn a_query_naming_another_dimension_set_answers_no_datapoints() {
    let router = emulator_with_cloudwatch();
    let table = "dimension-identity";
    create_table(&router, table).await;
    describe_when_active(&router, table, table).await;
    let case = "a write read back under every dimension set";

    ok(
        &router,
        "PutItem",
        json!({"TableName": table, "Item": item(&key(0), 1_013)}),
    )
    .await;

    // The series the write really published, so the rows below are about the dimension set
    // and not about a table nothing wrote to.
    let consumption = series_sum(&router, table, CONSUMED_WRITE, None).await;
    assert!(
        (consumption - 1.0).abs() < TOLERANCE,
        "case {case:?}: expected the write of a one-kilobyte item to carry one unit, got \
         {consumption}"
    );

    // (the case, the metric, the dimension set the query names)
    let rows: [(&str, &str, Value); 4] = [
        (
            "the consumption family under the latency family's pair",
            CONSUMED_WRITE,
            dimensions(table, Some("PutItem")),
        ),
        (
            "the latency family under the table alone",
            LATENCY,
            dimensions(table, None),
        ),
        (
            "the consumption family of a table nothing wrote to",
            CONSUMED_WRITE,
            dimensions("another-table", None),
        ),
        (
            "the latency family of an operation nothing was made with",
            LATENCY,
            dimensions(table, Some("DeleteItem")),
        ),
    ];
    for (row, metric, named) in rows {
        let results = read(&router, metric, named.clone(), "Sum").await;
        let values: Vec<Value> = results
            .iter()
            .flat_map(|result| {
                result["Values"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
            })
            .collect();
        assert!(
            values.is_empty(),
            "case {case:?}, {row:?}: expected no datapoints for {metric} under {named}, got \
             {values:?}"
        );
    }

    // The unit a latency datapoint carries, read where the series really is: a `MetricStat`
    // may filter on it, so a query naming the right unit answers the samples and one naming
    // another unit answers none.
    for (unit, expected) in [(MILLISECONDS, 1.0), ("Count", 0.0)] {
        let filtered = read_with_unit(&router, table, "PutItem", unit).await;
        assert!(
            (filtered - expected).abs() < TOLERANCE,
            "case {case:?}: expected {expected} {unit} latency samples, got {filtered}"
        );
    }
}

/// The payload-free key of the item at `position` of a row.
fn key(position: usize) -> String {
    format!("k{position}")
}

/// One item under `key`, with `payload` bytes of payload beside it — or the key alone when
/// the row asks for no payload.
fn item(key: &str, payload: usize) -> Value {
    let mut written = json!({KEY_ATTRIBUTE: {"S": key}});
    if payload > 0 {
        written[PAYLOAD_ATTRIBUTE] = json!({"S": "x".repeat(payload)});
    }
    written
}

/// The stored size of [`item`], by the rule that sizes one: every attribute's name plus its
/// value, a string value counting its raw byte length.
fn item_size(key: &str, payload: usize) -> i64 {
    let mut size = KEY_ATTRIBUTE.len() + key.len();
    if payload > 0 {
        size += PAYLOAD_ATTRIBUTE.len() + payload;
    }
    i64::try_from(size).expect("an item of this suite fits in the size type")
}

/// Create one on-demand table with a single-attribute key. The wait for `ACTIVE` is the
/// caller's, so twin tables can be created before either is waited for.
async fn create_table(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "AttributeDefinitions": [{"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
}

/// One DynamoDB call that must be served, answering its parsed body.
async fn answered(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, parsed) = call(router, operation, body.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} {body} to answer 200, got {status} {parsed:?}"
    );
    parsed
}

/// Every `CapacityUnits` figure a response carries, summed: with `ReturnConsumedCapacity` of
/// `TOTAL` each entry carries one aggregate for the table it is about
/// (`ConsumedCapacity$CapacityUnits`), and these rows address one table per request.
fn reported_units(answered: &Value) -> f64 {
    match answered {
        Value::Object(members) => members
            .iter()
            .map(|(name, value)| {
                if name == "CapacityUnits" {
                    value.as_f64().unwrap_or_default()
                } else {
                    reported_units(value)
                }
            })
            .sum(),
        Value::Array(entries) => entries.iter().map(reported_units).sum(),
        _ => 0.0,
    }
}

/// The dimension set a series of `table` is published under: the table alone for the
/// consumption family, the table and the API for the latency family.
fn dimensions(table: &str, operation: Option<&str>) -> Value {
    let mut named = vec![json!({"Name": TABLE_NAME, "Value": table})];
    if let Some(operation) = operation {
        named.push(json!({"Name": OPERATION, "Value": operation}));
    }
    Value::Array(named)
}

/// The figures `GetMetricData` folds out of one series, summed over the windows it answered:
/// the units a consumption series carries, or the sample count of a latency series.
async fn series_sum(
    router: &MultiServiceRouter,
    table: &str,
    metric: &str,
    operation: Option<&str>,
) -> f64 {
    let statistic = if metric == LATENCY { "SampleCount" } else { "Sum" };
    let results = read(router, metric, dimensions(table, operation), statistic).await;
    values(&results).iter().sum()
}

/// Every result `GetMetricData` answers for the latency series of `operation` on `table`,
/// folded by `Average` — the elapsed times of the requests it booked.
async fn latency_results(
    router: &MultiServiceRouter,
    table: &str,
    operation: &str,
) -> Vec<Value> {
    read(
        router,
        LATENCY,
        dimensions(table, Some(operation)),
        "Average",
    )
    .await
}

/// How many latency samples a query answers when it filters on `unit`.
async fn read_with_unit(
    router: &MultiServiceRouter,
    table: &str,
    operation: &str,
    unit: &str,
) -> f64 {
    let answered = get_metric_data(
        router,
        json!([{
            "Id": "m1",
            "MetricStat": {
                "Metric": {
                    "Namespace": NAMESPACE,
                    "MetricName": LATENCY,
                    "Dimensions": dimensions(table, Some(operation)),
                },
                "Period": PERIOD,
                "Stat": "SampleCount",
                "Unit": unit,
            },
        }]),
    )
    .await;
    values(&results_of(&answered)).iter().sum()
}

/// Every value of `results`, in the order the response listed them.
fn values(results: &[Value]) -> Vec<f64> {
    results
        .iter()
        .flat_map(|result| {
            result["Values"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_f64())
                .collect::<Vec<f64>>()
        })
        .collect()
}

/// One `MetricStat` query over `metric` under `named`, folded by `statistic`.
async fn read(
    router: &MultiServiceRouter,
    metric: &str,
    named: Value,
    statistic: &str,
) -> Vec<Value> {
    let answered = get_metric_data(
        router,
        json!([{
            "Id": "m1",
            "MetricStat": {
                "Metric": {
                    "Namespace": NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": named,
                },
                "Period": PERIOD,
                "Stat": statistic,
            },
        }]),
    )
    .await;
    results_of(&answered)
}

/// Send `queries` to `GetMetricData` over a window wide enough to hold everything this suite
/// published, ordered oldest first.
async fn get_metric_data(router: &MultiServiceRouter, queries: Value) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    ok_cloudwatch(
        router,
        "GetMetricData",
        json!({
            "MetricDataQueries": queries,
            "StartTime": now - REACH,
            "EndTime": now + REACH,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await
}

/// The `MetricDataResults` of one response.
fn results_of(answered: &Value) -> Vec<Value> {
    answered["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("GetMetricData answers MetricDataResults, got {answered}"))
}
