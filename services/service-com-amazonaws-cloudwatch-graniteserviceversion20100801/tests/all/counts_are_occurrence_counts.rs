// SPDX-License-Identifier: CC0-1.0

//! What a `Counts` array may hold, and what a publish carrying a number that is no count is
//! answered with.
//!
//! The rule under test: **a `Counts` entry is a number of OCCURRENCES, so every entry must be
//! greater than 0 — a batch carrying one that is not is refused as the modelled
//! parameter-value fault naming that datum's `Counts` member, nothing of the batch is stored,
//! and a read of the metric afterwards reports no datapoint at all.** "Each number in the
//! `Count` array is the number of times the corresponding value in the `Values` array occurred
//! during the period." (`smithy_query(query="doc",shape="MetricDatum",member="Counts")`)
//!
//! The rows of the refused table differ along what the rule generalises over: WHICH number is
//! no count (zero itself, negative zero, the largest double just below zero, a small and a
//! large negative), WHERE in the `Counts` array it sits (the only entry, the first, the last,
//! one in the middle, every entry), how long the arrays are, and WHERE in the request the
//! offending datum sits — the first datum of a batch, a later one, and one carried on
//! `EntityMetricData` rather than on `MetricData` — since the member path the sentence names is
//! read off the request. The negative first count of a two-value histogram is one row of that
//! table, not the requirement.
//!
//! The accepted side is a table too, and it is what makes a refusal attributable to the count
//! alone: a count greater than 0 is ingested and weights the value beside it, including the
//! fractions of an occurrence that sit between the bound and one. The accepted form at large —
//! whole counts, an omitted `Counts` array — is `crate::probe_counts`'s, so it is not repeated
//! here beyond the one control row.
//!
//! The last test is the AGGREGATION half of the same rule, and it is reached from the other
//! writer of this metric store (the services that publish their own telemetry into it), since a
//! publish can no longer place such a window: **a window whose datapoints stand for no sample
//! defines no statistic, so no reader reports a datapoint for it** — asserted through
//! `GetMetricStatistics` and `GetMetricData`, beside a window of the same series that does hold
//! samples and is reported.

use serde_json::{json, Value};
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state::{
    ComAmazonawsCloudwatchGraniteserviceversion20100801State as CloudWatchState, MetricKey,
    StoredDatum,
};

use crate::harness::{
    build_router, build_router_with_engine, call_in_query_mode, call_with_headers, error_message,
    error_type, now_secs, ok,
};

/// The namespace every publish in this suite names.
const NAMESPACE: &str = "Probe/OccurrenceCounts";

/// One minute, in the epoch seconds a published time stamp carries.
const MINUTE: f64 = 60.0;

/// The header a query-compatible client reads the legacy error report in.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The qualified shape id a refusal body names the parameter-value fault under.
const PARAMETER_VALUE_SHAPE_ID: &str = "com.amazon.coral.service#InvalidParameterValueException";

/// The report that fault's legacy `awsQueryError` code and side are carried as.
const PARAMETER_VALUE_REPORT: &str = "InvalidParameterValue;Sender";

/// The name every offending datum of this suite publishes under, so one read answers whether
/// the refused batch left anything behind.
const REFUSED_METRIC: &str = "RefusedHistogram";

/// A minute-aligned instant ten minutes in the past, so every published datapoint sits inside
/// each retention tier and a read's start time rounds to where it was put.
fn base_time() -> f64 {
    (now_secs() / MINUTE).floor() * MINUTE - 10.0 * MINUTE
}

/// The sentence a refusal of the array at `parameter` carries: the rule's own fixed text, with
/// the request member path the array arrived on as its only variable part.
fn counts_message(parameter: &str) -> String {
    format!("The parameter {parameter} must contain only values greater than 0.")
}

/// A datum publishing `values` for `metric` at `at`, weighted by `counts` when it carries any.
fn histogram(metric: &str, at: f64, values: &[f64], counts: Option<&[f64]>) -> Value {
    let mut datum = json!({
        "MetricName": metric,
        "Timestamp": at,
        "Values": values,
    });
    if let Some(counts) = counts {
        datum["Counts"] = json!(counts);
    }
    datum
}

/// `position - 1` well-formed publishes followed by `offending`, so the position a refusal names
/// is the offending datum's own rather than a fixed place.
fn batch_ending_with(at: f64, position: usize, offending: Value) -> Value {
    let mut data: Vec<Value> = (1..position)
        .map(|index| {
            json!({
                "MetricName": format!("Filler{index}"),
                "Timestamp": at,
                "Value": 1.0,
            })
        })
        .collect();
    data.push(offending);
    Value::Array(data)
}

/// A publish of `data` on `MetricData`.
fn publish(data: Value) -> Value {
    json!({"Namespace": NAMESPACE, "MetricData": data})
}

/// The same data carried on the OTHER list a publish may use, under one entity.
fn publish_as_entity_data(data: Value) -> Value {
    json!({
        "Namespace": NAMESPACE,
        "StrictEntityValidation": false,
        "EntityMetricData": [{
            "Entity": {"KeyAttributes": {"Type": "Service", "Name": "probe"}},
            "MetricData": data,
        }],
    })
}

/// One refused publish: what it varies, the request, and the member path the refusal must name.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The whole publish the row sends.
    request: Value,
    /// The member path the sentence names.
    parameter: String,
}

/// Every row of the refused table.
fn refused_rows(at: f64) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();

    // WHICH number is no count, and where in the array it sits — each on the first datum of a
    // one-datum batch. `-f64::MIN_POSITIVE` is the double just below the bound: the nearest
    // number to zero that is still not greater than it.
    for (case, values, counts) in [
        ("a negative count first", vec![1.0, 2.0], vec![-1.0, 1.0]),
        ("a negative count last", vec![1.0, 2.0], vec![1.0, -1.0]),
        ("zero as the only count", vec![4.0], vec![0.0]),
        ("negative zero as the only count", vec![4.0], vec![-0.0]),
        (
            "the double just below the bound",
            vec![4.0],
            vec![-f64::MIN_POSITIVE],
        ),
        (
            "zero among positive counts",
            vec![1.0, 2.0, 3.0, 4.0],
            vec![1.0, 2.0, 0.0, 4.0],
        ),
        (
            "every count non-positive",
            vec![1.0, 2.0, 3.0],
            vec![-1.0, -2.0, -3.0],
        ),
        (
            "a large negative count",
            vec![5.0, 6.0, 7.0],
            vec![2.0, -1.0e6, 3.0],
        ),
    ] {
        rows.push(Row {
            case,
            request: publish(Value::Array(vec![histogram(
                REFUSED_METRIC,
                at,
                &values,
                Some(&counts),
            )])),
            parameter: "MetricData.member.1.Counts".to_string(),
        });
    }

    // The same array further along the batch: the position the sentence names is the datum's.
    for (case, position) in [
        ("the offending datum second in its batch", 2),
        ("the offending datum fourth in its batch", 4),
    ] {
        rows.push(Row {
            case,
            request: publish(batch_ending_with(
                at,
                position,
                histogram(REFUSED_METRIC, at, &[1.0, 2.0], Some(&[-1.0, 1.0])),
            )),
            parameter: format!("MetricData.member.{position}.Counts"),
        });
    }

    // The same array on the other list a publish may use, at either depth of its nesting.
    rows.push(Row {
        case: "the offending datum carried as entity metric data",
        request: publish_as_entity_data(Value::Array(vec![histogram(
            REFUSED_METRIC,
            at,
            &[1.0, 2.0],
            Some(&[0.0, 1.0]),
        )])),
        parameter: "EntityMetricData.member.1.MetricData.member.1.Counts".to_string(),
    });
    rows.push(Row {
        case: "the offending datum second in an entity's data",
        request: publish_as_entity_data(batch_ending_with(
            at,
            2,
            histogram(REFUSED_METRIC, at, &[1.0, 2.0], Some(&[1.0, -3.0])),
        )),
        parameter: "EntityMetricData.member.1.MetricData.member.2.Counts".to_string(),
    });

    rows
}

/// The `(SampleCount, Sum, Minimum, Maximum)` one window of a series folds to.
type Aggregate = (f64, f64, f64, f64);

/// One accepted publish: what it varies, the values, the counts beside them when it carries
/// any, and the aggregate its window must fold to.
struct Accepted {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The values the datum publishes.
    values: Vec<f64>,
    /// The counts weighting them, or `None` for a datum that carries no `Counts` array.
    counts: Option<Vec<f64>>,
    /// The aggregate the window holding them must fold to.
    expected: Aggregate,
}

/// The datapoints the window at `at` holds for `metric`, each as the aggregate it folds to.
async fn window_folds(
    router: &smithy_server_runtime::MultiServiceRouter,
    metric: &str,
    at: f64,
) -> Vec<Aggregate> {
    let body = ok(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "StartTime": at,
            "EndTime": at + MINUTE,
            "Period": 60,
            "Statistics": ["SampleCount", "Sum", "Minimum", "Maximum"],
        }),
    )
    .await;
    body["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            let number = |member: &str| point.get(member).and_then(Value::as_f64);
            (
                number("SampleCount").unwrap_or(f64::NAN),
                number("Sum").unwrap_or(f64::NAN),
                number("Minimum").unwrap_or(f64::NAN),
                number("Maximum").unwrap_or(f64::NAN),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_count_that_is_not_greater_than_zero_is_refused_naming_the_datums_counts_member() {
    let at = base_time();

    for Row {
        case,
        request,
        parameter,
    } in refused_rows(at)
    {
        // A fresh emulator per row, so what the row's own publish left behind is the whole
        // store the ingest assertions below read.
        let router = build_router();

        // 1. A query-compatible client reads the modelled fault in both places.
        let (status, headers, body) = call_in_query_mode(&router, "PutMetricData", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?} (request {request}): expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            PARAMETER_VALUE_SHAPE_ID,
            "case {case:?}: expected the body to name {PARAMETER_VALUE_SHAPE_ID}, got {body}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, PARAMETER_VALUE_REPORT,
            "case {case:?}: expected {QUERY_ERROR_HEADER} to read {PARAMETER_VALUE_REPORT:?}, \
             got {reported:?}"
        );

        // 2. The sentence names the member path the offending array arrived on.
        let expected = counts_message(&parameter);
        let stated = error_message(&body);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );

        // 3. A native JSON client gets the same answer and no legacy report.
        let (plain_status, plain_headers, plain_body) =
            call_with_headers(&router, "PutMetricData", &request).await;
        assert_eq!(
            (plain_status, error_type(&plain_body)),
            (400, PARAMETER_VALUE_SHAPE_ID.to_string()),
            "case {case:?}: a native JSON client must get the same answer, got {plain_status} \
             with {plain_body}"
        );
        assert!(
            plain_headers.get(QUERY_ERROR_HEADER).is_none(),
            "case {case:?}: a client that did not opt in must get no report, got {:?}",
            plain_headers.get(QUERY_ERROR_HEADER)
        );

        // 4. "Any validation error (for entity or metric data) will fail the entire request,
        // and no data will be ingested." (`PutMetricDataInput$StrictEntityValidation`) — not
        // even the well-formed data that rode along, so the batch created no series at all.
        let listed = ok(&router, "ListMetrics", &json!({"Namespace": NAMESPACE})).await;
        assert_eq!(
            listed["Metrics"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: a refused publish must store no series at all, got {listed}"
        );

        // 5. And the read that follows it reports no datapoint for the refused metric: a
        // period nothing was stored for aggregates nothing.
        let folds = window_folds(&router, REFUSED_METRIC, at).await;
        assert!(
            folds.is_empty(),
            "case {case:?}: expected no datapoint for a refused batch, got {folds:?}"
        );
    }
}

#[tokio::test]
async fn every_count_greater_than_zero_is_accepted_and_weights_its_value() {
    let router = build_router();
    let at = base_time();

    // The accepted histogram form at large — whole counts, a count of one everywhere, an
    // omitted `Counts` array — is pinned by `crate::probe_counts`; these rows are the SIDE OF
    // THE BOUND that suite does not reach: the bound is "greater than 0", not "at least one",
    // so a fraction of an occurrence is a count the service holds and weights its value by.
    // One whole count rides along as the control the refusals above are attributed against.
    let rows: Vec<Accepted> = vec![
        Accepted {
            case: "a fractional count on every value",
            values: vec![2.0, 4.0],
            counts: Some(vec![0.5, 0.5]),
            expected: (1.0, 3.0, 2.0, 4.0),
        },
        Accepted {
            case: "a quarter of an occurrence",
            values: vec![8.0],
            counts: Some(vec![0.25]),
            expected: (0.25, 2.0, 8.0, 8.0),
        },
        Accepted {
            case: "a fraction beside a whole count",
            values: vec![2.0, 4.0],
            counts: Some(vec![0.5, 3.0]),
            expected: (3.5, 13.0, 2.0, 4.0),
        },
        Accepted {
            case: "whole counts above one",
            values: vec![1.0, 2.0, 3.0],
            counts: Some(vec![10.0, 1.0, 1.0]),
            expected: (12.0, 15.0, 1.0, 3.0),
        },
    ];

    for (
        index,
        Accepted {
            case,
            values,
            counts,
            expected,
        },
    ) in rows.into_iter().enumerate()
    {
        let metric = format!("Accepted{index}");
        let (status, body) = crate::harness::call(
            &router,
            "PutMetricData",
            &publish(Value::Array(vec![histogram(
                &metric,
                at,
                &values,
                counts.as_deref(),
            )])),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a histogram whose counts are all greater than 0 must be accepted, \
             got {status} {body}"
        );

        let folds = window_folds(&router, &metric, at).await;
        assert_eq!(
            folds,
            vec![expected],
            "case {case:?}: expected the window to fold to {expected:?}, got {folds:?}"
        );
    }
}

/// Store `data` directly in the `metric` series of [`NAMESPACE`], the way the services that
/// publish their OWN telemetry into this store write it
/// (`State::append_datapoints`) — the writer that can place a window `PutMetricData` cannot,
/// now that it refuses a count that is not greater than 0, so the aggregation half of the rule
/// is reached from here.
fn seed(engine: &engine::InMemoryEngine, metric: &str, data: Vec<StoredDatum>) {
    let key = MetricKey {
        namespace: NAMESPACE.to_string(),
        metric_name: metric.to_string(),
        dimensions: std::collections::BTreeMap::new(),
    };
    engine
        .state::<CloudWatchState>()
        .lock()
        .append_datapoints(key, data)
        .expect("the metric store has room for the seeded series");
}

/// One stored datapoint standing for `sample_count` observations summing to `sum`, spanning
/// `minimum` to `maximum`, at `at` — at the default storage resolution a published datum has.
fn stored(at: f64, sample_count: f64, sum: f64, minimum: f64, maximum: f64) -> StoredDatum {
    StoredDatum {
        timestamp_secs: at,
        sample_count,
        sum,
        minimum,
        maximum,
        unit: None,
        storage_resolution: 60,
    }
}

#[tokio::test]
async fn a_window_standing_for_no_sample_is_reported_by_no_reader() {
    let (router, engine) = build_router_with_engine();
    let at = base_time();
    let metric = "SeededWindows";

    // The first window stands for NO sample — the shape an unchecked count once admitted: a
    // sum and a span with a sample count of zero. The second is the same series one window
    // later, standing for two samples, so what the readers below withhold is the first window
    // rather than the series. The third window's two datapoints cancel each other's sample
    // counts, which is the same window by another route.
    seed(
        &engine,
        metric,
        vec![
            stored(at, 0.0, -1.0, 1.0, 2.0),
            stored(at + MINUTE, 2.0, 12.0, 5.0, 7.0),
            stored(at + 2.0 * MINUTE, 3.0, 9.0, 3.0, 3.0),
            stored(at + 2.0 * MINUTE, -3.0, -9.0, 3.0, 3.0),
        ],
    );

    // 1. `GetMetricStatistics` reports the window that holds samples and no other.
    let body = ok(
        &router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "StartTime": at,
            "EndTime": at + 3.0 * MINUTE,
            "Period": 60,
            "Statistics": ["SampleCount", "Sum", "Minimum", "Maximum"],
        }),
    )
    .await;
    let reported: Vec<(f64, Aggregate)> = body["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            let number = |member: &str| point.get(member).and_then(Value::as_f64);
            (
                number("Timestamp").unwrap_or(f64::NAN),
                (
                    number("SampleCount").unwrap_or(f64::NAN),
                    number("Sum").unwrap_or(f64::NAN),
                    number("Minimum").unwrap_or(f64::NAN),
                    number("Maximum").unwrap_or(f64::NAN),
                ),
            )
        })
        .collect();
    assert_eq!(
        reported,
        vec![(at + MINUTE, (2.0, 12.0, 5.0, 7.0))],
        "case \"a sample-less window beside one that holds samples\": expected only the window \
         at {} to be reported, got {reported:?}",
        at + MINUTE
    );

    // 2. And so does `GetMetricData`, which folds the same series through the same windows.
    let body = ok(
        &router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{
                "Id": "m1",
                "MetricStat": {
                    "Metric": {"Namespace": NAMESPACE, "MetricName": metric},
                    "Period": 60,
                    "Stat": "Sum",
                },
            }],
            "StartTime": at,
            "EndTime": at + 3.0 * MINUTE,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    let result = &body["MetricDataResults"][0];
    let stamps: Vec<f64> = result["Timestamps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    let values: Vec<f64> = result["Values"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    assert_eq!(
        (stamps, values),
        (vec![at + MINUTE], vec![12.0]),
        "case \"the same series read through GetMetricData\": expected only the window at {} to \
         be reported, got {body}",
        at + MINUTE
    );
}

