// SPDX-License-Identifier: CC0-1.0

//! WHICH data a one-second `Period` selects, end to end over the wire.
//!
//! ONE rule, asserted over a table: **a read at the one-second period resolves per-second detail,
//! which only high-resolution storage keeps, so it is answered from the data stored AT that
//! resolution and from nothing else — "Setting this to 1 specifies this metric as a
//! high-resolution metric, so that CloudWatch stores the metric with sub-minute resolution down to
//! one second. Setting this to 60 specifies this metric as a regular-resolution metric, which
//! CloudWatch stores at 1-minute resolution… This field is optional, if you do not specify it the
//! default of 60 is used."
//! (`smithy_query(query="doc",shape="MetricDatum",member="StorageResolution")`) against
//! "High-resolution metrics are those metrics stored by a PutMetricData call that includes a
//! StorageResolution of 1 second."
//! (`smithy_query(query="doc",shape="GetMetricStatisticsInput",member="Period")`). Every period at
//! or above the divisions of a minute a read's windows are laid on (5, 10, 20, 30, the minute
//! itself and its multiples) answers from a datum of EITHER storage class, at the span that class
//! stores it in.**
//!
//! So the storage class a datum was published with decides whether a sub-second-resolving read can
//! see it at all, and it is a property of the DATUM rather than of the series or of the call: two
//! data published by one `PutMetricData` differ in what a one-second read answers from, which is
//! the recorded shape this suite opens with.
//!
//! The rows differ along what the rule generalises over: the storage class published (1, 60, and
//! the omitted member that is documented to read as 60), the period the read asks for (every
//! sub-minute value `MetricStat$Period` admits, the minute itself and a multiple of it), and where
//! the publish instant sits relative to the clock (this instant, a minute that closed well before
//! this run, and a minute still ahead of it — `PutMetricData` takes a time stamp "as much as 2
//! hours after the current day and time"). Both readers of a series are driven, because one fold
//! answers for both.
//!
//! Nothing here waits on the clock and no row's outcome depends on how fast the host runs: every
//! expectation follows from the time stamp the publish carried.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, now_secs, ok};

/// One minute, in the epoch seconds a published time stamp carries.
const MINUTE: f64 = 60.0;

/// The namespace every datum of this suite is published under.
const NAMESPACE: &str = "MyApp/PerSecondPeriod";

/// The value every observation carries, so a datum that IS answered is recognisable by its `Sum`.
const VALUE: f64 = 42.0;

/// How far either side of the publish instant every read's range reaches, in seconds: the window a
/// client that has just published one observation reads it back over.
const HALF_WINDOW: f64 = 30.0;

/// How far from this run's own minute the two displaced instants lie, in minutes: far enough that
/// a host taking minutes over this suite still reads the one as past and the other as ahead, and
/// well inside both the two-hour limit on a published time stamp and the three-hour tier a
/// sub-minute period is served from.
const DISPLACEMENT_MINUTES: f64 = 10.0;

/// The storage class a datum is published with, and whether it is the HIGH-resolution one: the two
/// values `MetricDatum$StorageResolution` documents, plus the omitted member — "if you do not
/// specify it the default of 60 is used".
const PUBLISHED_CLASSES: [(&str, Option<i64>, bool); 3] = [
    ("StorageResolution 1", Some(1), true),
    ("StorageResolution 60", Some(60), false),
    ("an omitted StorageResolution", None, false),
];

/// The period a read asks for, and whether it resolves the per-second detail only high-resolution
/// storage keeps: every sub-minute value `MetricStat$Period` admits ("the period can be 1, 5, 10,
/// 20, 30, 60, or any multiple of 60"), the minute itself and a multiple of it.
const PERIODS: [(i64, bool); 7] = [
    (1, true),
    (5, false),
    (10, false),
    (20, false),
    (30, false),
    (60, false),
    (300, false),
];

/// Publish one datum per storage class, all at `timestamp` and all in ONE `PutMetricData` call —
/// the recorded shape — each under a metric name of its own so a read answers from one of them.
async fn publish_one_of_each_class(router: &MultiServiceRouter, prefix: &str, timestamp: f64) {
    let data: Vec<Value> = PUBLISHED_CLASSES
        .iter()
        .map(|(_, resolution, _)| {
            let mut datum = json!({
                "MetricName": metric_name(prefix, *resolution),
                "Value": VALUE,
                "Unit": "Count",
                "Timestamp": timestamp,
            });
            if let Some(resolution) = resolution {
                datum["StorageResolution"] = json!(resolution);
            }
            datum
        })
        .collect();
    ok(
        router,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": data}),
    )
    .await;
}

/// The metric name the datum of `resolution` is published under, inside the series group `prefix`.
fn metric_name(prefix: &str, resolution: Option<i64>) -> String {
    format!("{prefix}Res{}", resolution.unwrap_or(0))
}

/// The `Sum` values `GetMetricStatistics` answers with for `metric` over `[start, end)` at
/// `period`, oldest datapoint first.
async fn statistics_sums(
    router: &MultiServiceRouter,
    metric: &str,
    start: f64,
    end: f64,
    period: i64,
) -> Vec<f64> {
    let body = ok(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "StartTime": start,
            "EndTime": end,
            "Period": period,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    body["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .collect()
}

/// The same read through `GetMetricData`, whose fold is the same one.
async fn metric_data_sums(
    router: &MultiServiceRouter,
    metric: &str,
    start: f64,
    end: f64,
    period: i64,
) -> Vec<f64> {
    let body = ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{
                "Id": "m1",
                "MetricStat": {
                    "Metric": {"Namespace": NAMESPACE, "MetricName": metric},
                    "Period": period,
                    "Stat": "Sum",
                },
            }],
            "StartTime": start,
            "EndTime": end,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    body["MetricDataResults"][0]["Values"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .collect()
}

/// RULE: a one-second read answers from the data stored at that resolution and from no other,
/// while every coarser period answers from either storage class.
#[tokio::test]
async fn a_per_second_read_answers_only_from_per_second_storage() {
    let router = build_router();
    let this_minute = (now_secs() / MINUTE).floor() * MINUTE;

    // (case, the instant the data is published at) — this instant, the minute that closed well
    // before this run, and a minute still ahead of it.
    let instants: [(&str, f64); 3] = [
        ("published at this instant", now_secs()),
        (
            "published in a minute that has closed",
            this_minute - DISPLACEMENT_MINUTES * MINUTE + 30.0,
        ),
        (
            "published in a minute still ahead",
            this_minute + DISPLACEMENT_MINUTES * MINUTE + 30.0,
        ),
    ];

    for (index, (phase, stamped)) in instants.into_iter().enumerate() {
        let prefix = format!("Twin{index}");
        publish_one_of_each_class(&router, &prefix, stamped).await;
        let (start, end) = (stamped - HALF_WINDOW, stamped + HALF_WINDOW);

        for (class, resolution, high_resolution) in PUBLISHED_CLASSES {
            let metric = metric_name(&prefix, resolution);
            for (period, resolves_per_second) in PERIODS {
                // The datum is answered unless the read resolves detail its storage class keeps
                // none of; where it is answered, the one observation's value is its `Sum`.
                let expected: Vec<f64> = if resolves_per_second && !high_resolution {
                    Vec::new()
                } else {
                    vec![VALUE]
                };
                let case = format!("{class} {phase}, read at Period {period}");
                for (reader, got) in [
                    (
                        "GetMetricStatistics",
                        statistics_sums(&router, &metric, start, end, period).await,
                    ),
                    (
                        "GetMetricData",
                        metric_data_sums(&router, &metric, start, end, period).await,
                    ),
                ] {
                    assert_eq!(
                        got, expected,
                        "case {case:?}: expected {reader} to answer {expected:?}, got {got:?}"
                    );
                }
            }
        }
    }
}

/// The two data of ONE `PutMetricData` call are told apart by the storage class each carried, not
/// by the call: the recorded shape, read at the one period that tells them apart and at the period
/// a regular-resolution series is stored at.
#[tokio::test]
async fn one_call_can_publish_data_of_both_classes_and_a_per_second_read_sees_one() {
    let router = build_router();
    let stamped = now_secs();
    let prefix = "Pair";
    publish_one_of_each_class(&router, prefix, stamped).await;
    let (start, end) = (stamped - HALF_WINDOW, stamped + HALF_WINDOW);

    let high = metric_name(prefix, Some(1));
    let regular = metric_name(prefix, Some(60));
    let defaulted = metric_name(prefix, None);

    // At the per-second period only the high-resolution datum of that call is answered.
    assert_eq!(
        statistics_sums(&router, &high, start, end, 1).await,
        vec![VALUE],
        "case {high:?}: the high-resolution datum of the call must answer a per-second read"
    );
    for metric in [&regular, &defaulted] {
        let got = statistics_sums(&router, metric, start, end, 1).await;
        assert!(
            got.is_empty(),
            "case {metric:?}: a regular-resolution datum of the same call keeps no per-second \
             detail, so a per-second read must answer with no datapoint, got {got:?}"
        );
    }

    // At the minute a regular-resolution datum IS stored at, every datum of the call answers.
    for metric in [&high, &regular, &defaulted] {
        let got = statistics_sums(&router, metric, start, end, 60).await;
        assert_eq!(
            got,
            vec![VALUE],
            "case {metric:?}: every datum of the call is answered at the minute, got {got:?}"
        );
    }
}
