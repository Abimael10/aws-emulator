// SPDX-License-Identifier: CC0-1.0

//! A `Counts` entry of ZERO: a number of occurrences like any other, and the value it counts
//! occurred none of them.
//!
//! The rule under test: **a `Counts` entry is the number of times the value beside it occurred
//! during the period, so zero is a count the publish is ACCEPTED under — it weights no sample
//! and nothing of its value into the period, while the value it counts still belongs to the
//! SPAN the datum published, which is what the window's `Minimum` and `Maximum` report.**
//! "Each number in the `Count` array is the number of times the corresponding value in the
//! `Values` array occurred during the period."
//! (`smithy_query(query="doc",shape="MetricDatum",member="Counts")`) So a `Values`/`Counts`
//! datum is the statistic set of what it carries: its sample count is its counts summed, its
//! sum is each value weighted by its own count, and its smallest and largest observation are
//! the smallest and largest of its values.
//!
//! A window that stands for no sample at all is no window of the series, so a datum whose
//! every count is zero creates the metric (`PutMetricData`: "If the specified metric does not
//! exist, CloudWatch creates the metric") and leaves no datapoint for a read to aggregate.
//!
//! The rows differ along what the rule generalises over: WHERE the zero sits in the array (the
//! only entry, the first, the last, one in the middle), HOW MANY of them there are (one, two,
//! every entry), how long the arrays are, WHICH value the zero counts (one below every counted
//! value, one above them, one equal to a counted one, so what the zero contributes to the
//! span differs row by row), what sits beside it (a whole count, a fraction of an occurrence),
//! and how the zero is spelled (`0`, `-0`). The negative counts of
//! `crate::counts_are_occurrence_counts` are the other side of the rule — the refusal — and one
//! of them rides along here as the control that makes this suite's acceptances attributable to
//! the zero rather than to the array.
//!
//! The second test is the same rule reached through the OTHER list a publish may carry data on
//! (`EntityMetricData`), since nothing about a count depends on which of the two the datum
//! arrived on.

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_type, now_secs, ok};

/// The namespace every publish in this suite names.
const NAMESPACE: &str = "Probe/ZeroCounts";

/// One minute, in the epoch seconds a published time stamp carries.
const MINUTE: f64 = 60.0;

/// A minute-aligned instant ten minutes in the past, so every published datapoint sits inside
/// each retention tier and a read's start time rounds to where it was put.
fn base_time() -> f64 {
    (now_secs() / MINUTE).floor() * MINUTE - 10.0 * MINUTE
}

/// The `(SampleCount, Sum, Minimum, Maximum)` one window of a series folds to.
type Aggregate = (f64, f64, f64, f64);

/// One accepted publish: what it varies, the histogram it carries, and the window it folds to.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The values the datum publishes.
    values: Vec<f64>,
    /// The counts weighting them.
    counts: Vec<f64>,
    /// The aggregate the window holding them must fold to, or `None` for a datum that counts
    /// no occurrence at all, which stands for no sample and so places no window.
    expected: Option<Aggregate>,
}

/// Every row of the accepted table. The expected aggregate of each is the statistic set of
/// the datum: its counts summed, each value weighted by its own count, and the smallest and
/// largest of the values it published.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "zero as the only count",
            values: vec![4.0],
            counts: vec![0.0],
            expected: None,
        },
        Row {
            case: "negative zero as the only count",
            values: vec![4.0],
            counts: vec![-0.0],
            expected: None,
        },
        Row {
            case: "every count zero",
            values: vec![1.0, 2.0, 3.0],
            counts: vec![0.0, 0.0, 0.0],
            expected: None,
        },
        Row {
            case: "zero first, counting a value below the counted one",
            values: vec![1.0, 2.0],
            counts: vec![0.0, 1.0],
            expected: Some((1.0, 2.0, 1.0, 2.0)),
        },
        Row {
            case: "zero last, counting a value above the counted one",
            values: vec![1.0, 2.0],
            counts: vec![1.0, 0.0],
            expected: Some((1.0, 1.0, 1.0, 2.0)),
        },
        Row {
            case: "zero in the middle of whole counts",
            values: vec![1.0, 2.0, 3.0, 4.0],
            counts: vec![2.0, 1.0, 0.0, 1.0],
            expected: Some((4.0, 8.0, 1.0, 4.0)),
        },
        Row {
            case: "two zeros, both counting the largest value",
            values: vec![10.0, 1.0, 10.0],
            counts: vec![0.0, 3.0, 0.0],
            expected: Some((3.0, 3.0, 1.0, 10.0)),
        },
        Row {
            case: "zero beside a fraction of an occurrence",
            values: vec![2.0, 4.0],
            counts: vec![0.0, 0.5],
            expected: Some((0.5, 2.0, 2.0, 4.0)),
        },
        Row {
            case: "zero counting a value another entry counts too",
            values: vec![5.0, 5.0],
            counts: vec![0.0, 2.0],
            expected: Some((2.0, 10.0, 5.0, 5.0)),
        },
    ]
}

/// A datum publishing `values` for `metric` at `at`, weighted by `counts`.
fn histogram(metric: &str, at: f64, values: &[f64], counts: &[f64]) -> Value {
    json!({
        "MetricName": metric,
        "Timestamp": at,
        "Values": values,
        "Counts": counts,
    })
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

/// Whether `ListMetrics` reports `metric` in this suite's namespace.
async fn metric_is_listed(
    router: &smithy_server_runtime::MultiServiceRouter,
    metric: &str,
) -> bool {
    let listed = ok(
        router,
        "ListMetrics",
        &json!({"Namespace": NAMESPACE, "MetricName": metric}),
    )
    .await;
    listed["Metrics"]
        .as_array()
        .is_some_and(|metrics| !metrics.is_empty())
}

#[tokio::test]
async fn a_count_of_zero_is_accepted_and_weights_no_sample() {
    let at = base_time();

    for (index, row) in rows().into_iter().enumerate() {
        // A fresh emulator per row, so what the row's own publish left behind is the whole
        // store the assertions below read.
        let router = build_router();
        let case = row.case;
        let metric = format!("Zero{index}");

        let (status, body) = call(
            &router,
            "PutMetricData",
            &json!({
                "Namespace": NAMESPACE,
                "MetricData": [histogram(&metric, at, &row.values, &row.counts)],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a count of zero counts no occurrence and is accepted, got {status} \
             {body}"
        );

        // The publish created the metric whether or not any of its values occurred.
        assert!(
            metric_is_listed(&router, &metric).await,
            "case {case:?}: an accepted publish creates the metric it names, and \
             ListMetrics reports no {metric:?}"
        );

        // And the window it published into reports the statistic set of what it carried.
        let folds = window_folds(&router, &metric, at).await;
        let expected: Vec<Aggregate> = row.expected.into_iter().collect();
        assert_eq!(
            folds, expected,
            "case {case:?}: expected the window to fold to {expected:?}, got {folds:?}"
        );

        // The control: the same arrays with that count made NEGATIVE are refused, so each
        // acceptance above is attributable to the zero rather than to the array it sits in.
        let negated: Vec<f64> = row
            .counts
            .iter()
            .map(|count| if *count <= 0.0 { -1.0 } else { *count })
            .collect();
        let (refused_status, refused_body) = call(
            &router,
            "PutMetricData",
            &json!({
                "Namespace": NAMESPACE,
                "MetricData": [histogram(&metric, at, &row.values, &negated)],
            }),
        )
        .await;
        assert_eq!(
            (refused_status, error_type(&refused_body).as_str()),
            (400, "com.amazon.coral.service#InvalidParameterValueException"),
            "case {case:?}: the same publish with a negative count must be refused, got \
             {refused_status} {refused_body}"
        );

        // The refusal stored nothing either, so the window is still the one the accepted
        // publish placed.
        let after = window_folds(&router, &metric, at).await;
        assert_eq!(
            after, expected,
            "case {case:?}: a refused publish changes no window, expected {expected:?}, got \
             {after:?}"
        );
    }
}

#[tokio::test]
async fn a_count_of_zero_is_accepted_on_entity_metric_data_too() {
    let router = build_router();
    let at = base_time();
    let case = "a zero count carried as entity metric data";
    let metric = "EntityZero";

    let (status, body) = call(
        &router,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "StrictEntityValidation": false,
            "EntityMetricData": [{
                "Entity": {"KeyAttributes": {"Type": "Service", "Name": "probe"}},
                "MetricData": [histogram(metric, at, &[1.0, 2.0, 3.0], &[0.0, 2.0, 0.0])],
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a count of zero is a count on either list a publish may use, got \
         {status} {body}"
    );

    let folds = window_folds(&router, metric, at).await;
    assert_eq!(
        folds,
        vec![(2.0, 4.0, 1.0, 3.0)],
        "case {case:?}: expected the window to report the statistic set of the datum, got \
         {folds:?}"
    );
}
