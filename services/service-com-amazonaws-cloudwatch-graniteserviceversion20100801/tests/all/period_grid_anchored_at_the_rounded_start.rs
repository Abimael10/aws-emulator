// SPDX-License-Identifier: CC0-1.0

//! Where the period grid of a metric read is ANCHORED, through both reads that lay one.
//!
//! THE RULE: the windows a metric read answers run in `Period`-long steps from the start time
//! CloudWatch rounded the request's to, and each window is stamped with its own start — so the
//! aggregates a range is split into are split on boundaries measured from that instant, not on
//! multiples of the period measured from the epoch. "CloudWatch rounds the specified time stamp
//! as follows: Start time less than 15 days ago - Round down to the nearest whole minute. […]
//! Start time between 15 and 63 days ago - Round down to the nearest 5-minute clock interval.
//! […] Start time greater than 63 days ago - Round down to the nearest 1-hour clock interval."
//! (`GetMetricStatisticsInput$StartTime`, in the same words as `GetMetricDataInput$StartTime`),
//! and "The time stamp used for the data point" (`Datapoint$Timestamp`) is that window's start.
//!
//! Both reads that lay such a grid answer the same stamps and the same per-window aggregates for
//! one range, because the grid is one rule: `GetMetricStatistics` over a series and
//! `GetMetricData` over a query naming it.
//!
//! The rows span what the rule generalises over: the period (one, two, five and ten minutes, and
//! an hour), how far past the rounding interval the request starts (on the mark, a whole minute
//! in, and seconds in), how many windows the range holds (one, two and ten), and the age band the
//! start falls in (recent, so the whole minute, and between 15 and 63 days ago, so the 5-minute
//! clock interval).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router_with_engine, now_secs, ok};

/// The namespace every series of this suite is published under.
const NAMESPACE: &str = "MyApp/PeriodAnchor";

/// One minute, in seconds.
const MINUTE: f64 = 60.0;

/// How many one-minute observations the recent series holds.
const SAMPLES: i32 = 10;

/// The mark the recent series is published from: a whole TEN-minute clock mark far enough back
/// that every range below has closed, and recent enough to sit in the "less than 15 days ago"
/// band and inside the window a publish may stamp into ("You can specify time stamps that are as
/// much as two weeks before the current date", `PutMetricData`).
///
/// Settled once per test binary so every reader of this suite measures against the same instant.
fn mark() -> f64 {
    static BASE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| {
        let ten_minutes = 10.0 * MINUTE;
        ((now_secs() - 20.0 * MINUTE) / ten_minutes).floor() * ten_minutes
    })
}

/// The value the observation of minute `minute` carries: a different number per minute, so a
/// window's `Sum` says exactly WHICH minutes landed in it.
fn value_of(minute: i32) -> f64 {
    f64::from(minute + 1)
}

/// How far past [`mark`] the observation of minute `minute` is stamped: inside that minute
/// rather than on its boundary.
fn stamped_at(minute: i32) -> f64 {
    f64::from(minute * 60) + 13.0
}

/// Publish one regular-resolution observation per minute from [`mark`], each stamped inside its
/// own minute rather than on its boundary.
async fn given_a_minute_series(router: &MultiServiceRouter, metric: &str) {
    for minute in 0..SAMPLES {
        ok(
            router,
            "PutMetricData",
            &json!({
                "Namespace": NAMESPACE,
                "MetricData": [{
                    "MetricName": metric,
                    "Value": value_of(minute),
                    "Timestamp": mark() + stamped_at(minute),
                }],
            }),
        )
        .await;
    }
}

/// The `(Timestamp, Sum)` pairs `GetMetricStatistics` answers for `metric` over `[start, end)` at
/// `period`, oldest first.
async fn statistics(
    router: &MultiServiceRouter,
    metric: &str,
    start: f64,
    end: f64,
    period: i64,
) -> Vec<(f64, f64)> {
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
    let mut points: Vec<(f64, f64)> = body["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            (
                point["Timestamp"].as_f64().unwrap_or_default(),
                point["Sum"].as_f64().unwrap_or_default(),
            )
        })
        .collect();
    points.sort_by(|left, right| left.0.total_cmp(&right.0));
    points
}

/// The same range read through `GetMetricData`, which lays the same grid.
async fn metric_data(
    router: &MultiServiceRouter,
    metric: &str,
    start: f64,
    end: f64,
    period: i64,
) -> Vec<(f64, f64)> {
    let body = ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{
                "Id": "anchored",
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
    let mut points: Vec<(f64, f64)> = stamps.into_iter().zip(values).collect();
    points.sort_by(|left, right| left.0.total_cmp(&right.0));
    points
}

#[tokio::test]
async fn the_windows_of_a_recent_range_step_from_the_minute_the_start_rounds_onto() {
    let (router, _engine) = build_router_with_engine();
    let metric = "Anchored";
    given_a_minute_series(&router, metric).await;
    let base = mark();

    // (case, period, how far past the mark the request starts, how far past it the range ends,
    // the windows expected as (seconds past the mark, Sum))
    type Row = (&'static str, i64, f64, f64, Vec<(f64, f64)>);
    let rows: Vec<Row> = vec![
        (
            "five minutes from two minutes past the mark",
            300,
            2.0 * MINUTE,
            12.0 * MINUTE,
            vec![(2.0 * MINUTE, 25.0), (7.0 * MINUTE, 27.0)],
        ),
        (
            "five minutes from seconds past that same minute",
            300,
            2.0 * MINUTE + 17.0,
            12.0 * MINUTE,
            vec![(2.0 * MINUTE, 25.0), (7.0 * MINUTE, 27.0)],
        ),
        (
            "two minutes from one minute past the mark, so odd-minute windows",
            120,
            MINUTE,
            11.0 * MINUTE,
            vec![
                (MINUTE, 5.0),
                (3.0 * MINUTE, 9.0),
                (5.0 * MINUTE, 13.0),
                (7.0 * MINUTE, 17.0),
                (9.0 * MINUTE, 10.0),
            ],
        ),
        (
            "ten minutes from two minutes past the mark holds one window",
            600,
            2.0 * MINUTE,
            12.0 * MINUTE,
            vec![(2.0 * MINUTE, 52.0)],
        ),
        (
            "a minute at a time from seconds past the mark",
            60,
            30.0,
            10.0 * MINUTE + 30.0,
            (0..SAMPLES)
                .map(|minute| (f64::from(minute * 60), value_of(minute)))
                .collect(),
        ),
        (
            "five minutes from the mark itself, where the grid and the clock coincide",
            300,
            0.0,
            10.0 * MINUTE,
            vec![(0.0, 15.0), (5.0 * MINUTE, 40.0)],
        ),
    ];

    for (case, period, start_offset, end_offset, expected) in rows {
        let expected: Vec<(f64, f64)> = expected
            .into_iter()
            .map(|(offset, sum)| (base + offset, sum))
            .collect();
        let got = statistics(
            &router,
            metric,
            base + start_offset,
            base + end_offset,
            period,
        )
        .await;
        assert_eq!(
            got, expected,
            "case {case:?}: expected GetMetricStatistics windows {expected:?}, got {got:?}"
        );
        // The same grid through the other read of the same series.
        let through_data = metric_data(
            &router,
            metric,
            base + start_offset,
            base + end_offset,
            period,
        )
        .await;
        assert_eq!(
            through_data, expected,
            "case {case:?}: expected GetMetricData to answer the same windows {expected:?}, got \
             {through_data:?}"
        );
    }
}

#[tokio::test]
async fn the_windows_of_an_older_range_step_from_the_five_minute_interval_its_age_rounds_onto() {
    let (router, engine) = build_router_with_engine();
    let metric = "AnchoredOlder";
    const DAY: f64 = 24.0 * 60.0 * 60.0;
    /// How many one-minute observations the seeded series holds: an hour and a half of them, so
    /// an hourly read splits them across two windows.
    const MINUTES: i32 = 90;

    // A 5-minute clock mark thirty days back that is NO multiple of an hour, so an hourly window
    // stamped on it could not come from a clock-hour grid. Thirty days puts the range in the
    // "between 15 and 63 days ago" band, whose interval is the 5-minute one.
    let base = ((now_secs() - 30.0 * DAY) / 300.0).floor() * 300.0 + 300.0;
    let base = if base.rem_euclid(3_600.0) == 0.0 {
        base + 300.0
    } else {
        base
    };

    // An instant this old is one no publish can stamp — `PutMetricData` takes a time stamp only
    // inside the window it documents — so the series is seeded the way the write that was
    // current then left it, and what the rows assert is the READ's grid.
    for minute in 0..MINUTES {
        seed_datapoint(&engine, metric, base + stamped_at(minute), 1.0);
    }

    // A request opening seconds past a whole minute inside that 5-minute interval rounds back to
    // the interval, and the hourly windows step from there.
    let start = base + 3.0 * MINUTE + 34.0;
    let end = base + 90.0 * MINUTE;
    let expected = vec![(base, 60.0), (base + 3_600.0, 30.0)];
    for (case, got) in [
        (
            "GetMetricStatistics",
            statistics(&router, metric, start, end, 3_600).await,
        ),
        (
            "GetMetricData",
            metric_data(&router, metric, start, end, 3_600).await,
        ),
    ] {
        assert_eq!(
            got, expected,
            "case {case:?}: expected the hourly windows {expected:?} laid from the 5-minute \
             interval {base}, got {got:?}"
        );
    }
}

/// Write one datapoint of `metric` straight into the store, at an instant a publish can no longer
/// stamp: the ingest window `PutMetricData` documents is about WHEN a datapoint may be written,
/// not about how long CloudWatch keeps one.
fn seed_datapoint(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    metric: &str,
    at_secs: f64,
    value: f64,
) {
    use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

    let key = cloudwatch::MetricKey {
        namespace: NAMESPACE.to_string(),
        metric_name: metric.to_string(),
        dimensions: std::collections::BTreeMap::new(),
    };
    engine
        .state::<cloudwatch::ComAmazonawsCloudwatchGraniteserviceversion20100801State>()
        .lock()
        .append_datapoints(
            key,
            vec![cloudwatch::StoredDatum {
                timestamp_secs: at_secs,
                sample_count: 1.0,
                sum: value,
                minimum: value,
                maximum: value,
                unit: None,
                // The default `StorageResolution` of a published datum.
                storage_resolution: 60,
            }],
        )
        .expect("the metric store has room for the seeded series");
}
