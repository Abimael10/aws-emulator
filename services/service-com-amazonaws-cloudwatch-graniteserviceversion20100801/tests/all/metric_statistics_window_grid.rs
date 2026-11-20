// SPDX-License-Identifier: CC0-1.0

//! The WHOLE PERIODS a `GetMetricStatistics` read answers with.
//!
//! THE RULE: a read is of whole periods. Each datapoint it answers aggregates one `Period`-long
//! window of the clock and is stamped with that window's own start ("The time stamp used for the
//! data point", `Datapoint$Timestamp`), so the window the range opens in is read WHOLE rather
//! than cut off at an instant in the middle of it — its datapoint is answered even though the
//! period began before the instant the caller named — and every stamp a read answers with is
//! therefore the start of a clock window of that period.
//!
//! Where the range opens is the documented rounding of the requested start: "CloudWatch rounds
//! the specified time stamp as follows: Start time less than 15 days ago - Round down to the
//! nearest whole minute. […] Start time between 15 and 63 days ago - Round down to the nearest
//! 5-minute clock interval. […] Start time greater than 63 days ago - Round down to the nearest
//! 1-hour clock interval. […] If you set Period to 5, 10, 20, or 30, the start time of your
//! request is rounded down to the nearest time that corresponds to even 5-, 10-, 20-, or
//! 30-second divisions of a minute. For example … If you make a query at 15:07:17 for the
//! previous 5 minutes of data, using a period of 5 seconds, you receive data timestamped between
//! 15:02:15 and 15:07:15." (`GetMetricStatisticsInput$StartTime`) — that example is this rule at a
//! sub-minute period: the stamps are the starts of that period's own clock windows, the first of
//! them before the instant the query named.
//!
//! What a client gets from it: a series published once per period is reported ON that period's
//! boundaries whatever instant the read started from, which is what lets a 5-minute cadence be
//! told from a 1-minute one, and the most recent period of a series is readable as soon as it has
//! closed rather than only from a read that happens to begin on its boundary.
//!
//! The rows span what the rule generalises over: the period (a minute, five minutes, a quarter of
//! an hour and an hour, plus the sub-minute periods the sentence above names), and how far into
//! its window the requested start falls (the boundary itself, a quarter, the middle and
//! three-quarters of the way in).

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, now_secs, ok};

/// The namespace every series of this suite is published under.
const NAMESPACE: &str = "MyApp/WindowGrid";

/// One minute, in seconds.
const MINUTE: f64 = 60.0;

/// The periods a row reads at, in seconds: the narrowest regular cadence, the one a basic EC2
/// instance metric is published at, a quarter of an hour and an hour — plus the sub-minute
/// periods, which the documented sentence rounds onto their own divisions of a minute. Every one
/// of them is a `Period` the model admits (`GetMetricStatisticsInput$Period`, range min 1, and
/// the sub-minute values the sentence above names).
const PERIODS: &[f64] = &[5.0, 10.0, 20.0, 30.0, 60.0, 300.0, 900.0, 3_600.0];

/// Where inside its own window a row's requested start falls, as a fraction of the period: the
/// boundary itself and three instants after it, so the window a read begins in is entered at
/// every part of it.
const START_PHASES: &[f64] = &[0.0, 0.25, 0.5, 0.75];

/// How far back the series each row reads is published: an hour, so every row's range sits
/// inside the "less than 15 days ago" band and well inside the ingest window a publish may stamp
/// into ("You can specify time stamps that are as much as two weeks before the current date",
/// `PutMetricData`).
const AGE_SECS: f64 = 3_600.0;

/// The value every observation carries. Its size is immaterial — what each case asserts is WHICH
/// window answered it — so one value serves every row.
const VALUE: f64 = 7.0;

/// The datapoints of one `GetMetricStatistics` answer as `(Timestamp, SampleCount)`, oldest
/// first.
async fn read_windows(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    period: f64,
    start: f64,
    end: f64,
) -> Vec<(f64, f64)> {
    let body = ok(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": "Service", "Value": "api"}],
            "StartTime": start,
            "EndTime": end,
            "Period": period,
            "Statistics": ["SampleCount"],
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
                point["SampleCount"].as_f64().unwrap_or_default(),
            )
        })
        .collect();
    points.sort_by(|left, right| left.0.total_cmp(&right.0));
    assert!(
        !points.is_empty(),
        "case {case:?}: the read must answer the window the series was published into, got \
         {body}"
    );
    points
}

/// Publish one observation stamped `at`, at the storage resolution a period of `period` can be
/// read at: "if you do not specify it the default of 60 is used"
/// (`MetricDatum$StorageResolution`), so a sub-minute window needs a high-resolution series.
async fn publish(router: &MultiServiceRouter, metric: &str, at: f64, period: f64) {
    ok(
        router,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [{
                "MetricName": metric,
                "Dimensions": [{"Name": "Service", "Value": "api"}],
                "Value": VALUE,
                "StorageResolution": if period < MINUTE { 1 } else { 60 },
                "Timestamp": at,
            }],
        }),
    )
    .await;
}

/// The start of the `period`-wide clock window `at` falls in, computed here rather than imported
/// so the expected grid is this suite's own.
fn window_start(at: f64, period: f64) -> f64 {
    (at / period).floor() * period
}

/// Rule: a datapoint is stamped with the start of the window it aggregates, so every stamp a
/// read answers with is the start of a clock window of the period it asked for, whatever instant
/// the request started from.
#[tokio::test]
async fn every_answered_window_is_stamped_on_a_multiple_of_the_period() {
    let router = build_router();
    for (index, period) in PERIODS.iter().enumerate() {
        for (phase_index, phase) in START_PHASES.iter().enumerate() {
            let case = format!("a period of {period} seconds entered {phase} of the way in");
            let metric = format!("Stamped{index}x{phase_index}");
            // The observation sits in the middle of a window well inside the range, so the
            // window it lands in is neither the one the range opens in nor the one it closes in.
            let boundary = window_start(now_secs() - AGE_SECS, *period);
            let at = boundary + period / 2.0;
            publish(&router, &metric, at, *period).await;
            let start = boundary - 2.0 * period + period * phase;
            let end = boundary + 3.0 * period;
            let points = read_windows(&router, &case, &metric, *period, start, end).await;
            for (stamp, _) in &points {
                assert_eq!(
                    stamp.rem_euclid(*period),
                    0.0,
                    "case {case:?}: a window of {period} seconds begins on a multiple of \
                     {period}, got {stamp} from {points:?}"
                );
            }
            assert_eq!(
                points,
                vec![(boundary, 1.0)],
                "case {case:?}: the observation at {at} belongs to the window at {boundary}, got \
                 {points:?}"
            );
        }
    }
}

/// Rule: a read is of whole periods, so the window the range opens in is read WHOLE — a request
/// that begins after the observation but inside its period still answers that period's datapoint.
#[tokio::test]
async fn the_window_the_requested_start_falls_inside_is_read_whole() {
    let router = build_router();
    for (index, period) in PERIODS.iter().enumerate() {
        let case = format!("a period of {period} seconds, started past the observation");
        let metric = format!("Whole{index}");
        let boundary = window_start(now_secs() - AGE_SECS, *period);
        // The observation is in the first tenth of its window and the read begins in the last
        // tenth of the same window — after the observation, inside its window.
        let at = boundary + period / 10.0;
        let start = boundary + period * 0.9;
        publish(&router, &metric, at, *period).await;
        let points = read_windows(&router, &case, &metric, *period, start, boundary + period).await;
        assert_eq!(
            points,
            vec![(boundary, 1.0)],
            "case {case:?}: the window [{boundary}, {}) is read whole, so the observation at \
             {at} is answered for a read begun at {start}, got {points:?}",
            boundary + period
        );
    }
}

/// Rule, read through the statistic itself: a series published once per period answers one
/// observation per window of that period, and a read at a WIDER period folds the observations of
/// every narrower window it covers into one — the windows being whole periods of the clock is
/// what makes the two agree.
#[tokio::test]
async fn a_wider_read_folds_the_windows_it_covers() {
    let router = build_router();
    let metric = "FoldedMinutes";
    // Ten minutes of one observation per minute, each stamped inside its own minute, published
    // into a span that begins on a 5-minute boundary.
    let base = window_start(now_secs() - AGE_SECS, 300.0);
    for minute in 0..10 {
        publish(&router, metric, base + f64::from(minute) * MINUTE + 7.0, MINUTE).await;
    }

    // Read at a minute: one observation per minute, each stamped on its own minute.
    let case = "ten minutes read a minute at a time";
    let per_minute = read_windows(&router, case, metric, MINUTE, base, base + 10.0 * MINUTE).await;
    let expected: Vec<(f64, f64)> = (0..10)
        .map(|minute| (base + f64::from(minute) * MINUTE, 1.0))
        .collect();
    assert_eq!(
        per_minute, expected,
        "case {case:?}: expected one observation per minute of [{base}, {}), got {per_minute:?}",
        base + 10.0 * MINUTE
    );

    // Read at five minutes, from an instant in the MIDDLE of the first of those windows: the
    // same observations, folded five to a window, stamped on the 5-minute boundaries.
    let case = "the same ten minutes read five minutes at a time";
    let folded = read_windows(
        &router,
        case,
        metric,
        300.0,
        base + 137.0,
        base + 10.0 * MINUTE,
    )
    .await;
    assert_eq!(
        folded,
        vec![(base, 5.0), (base + 300.0, 5.0)],
        "case {case:?}: expected the minutes folded five to a 5-minute window stamped on the \
         5-minute boundaries, got {folded:?}"
    );
}
