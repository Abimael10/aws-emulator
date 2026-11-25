// SPDX-License-Identifier: CC0-1.0

//! The WHOLE PERIODS a `GetMetricStatistics` read answers with, and where their grid is anchored.
//!
//! THE RULE: a read is of whole periods laid FROM the start time CloudWatch rounded the request's
//! to. "CloudWatch rounds the specified time stamp as follows: Start time less than 15 days ago -
//! Round down to the nearest whole minute. […] Start time between 15 and 63 days ago - Round down
//! to the nearest 5-minute clock interval. […] Start time greater than 63 days ago - Round down to
//! the nearest 1-hour clock interval. […] If you set Period to 5, 10, 20, or 30, the start time of
//! your request is rounded down to the nearest time that corresponds to even 5-, 10-, 20-, or
//! 30-second divisions of a minute. For example … If you make a query at 15:07:17 for the previous
//! 5 minutes of data, using a period of 5 seconds, you receive data timestamped between 15:02:15
//! and 15:07:15." (`GetMetricStatisticsInput$StartTime`) — that rounding is the WHOLE of the
//! alignment: each datapoint aggregates one `Period`-long window of the grid that starts at the
//! rounded instant and is stamped with that window's own start ("The time stamp used for the data
//! point", `Datapoint$Timestamp`), as the example's stamps a period apart from 15:02:15 show. So a
//! stamp is a whole number of periods from the rounded start, and NOT in general a multiple of the
//! period measured from the epoch.
//!
//! What a client gets from it: the window a read reports is the window it asked for, so two reads
//! of the same series over the same range agree, and a client that windows a series by its own
//! `StartTime` reads aggregates split exactly on the boundaries it named rather than on boundaries
//! the clock chose for it.
//!
//! The rows span what the rule generalises over: the period (a minute, five minutes, a quarter of
//! an hour and an hour, plus the sub-minute periods the sentence above names), and how far past
//! the rounding interval the requested start falls (the interval itself, a quarter, the middle and
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

/// The start of the `span`-wide clock window `at` falls in.
fn floored(at: f64, span: f64) -> f64 {
    (at / span).floor() * span
}

/// The interval a requested start time of THIS suite's age is rounded down onto: the period
/// itself for the four sub-minute periods the documented sentence names ("If you set Period to 5,
/// 10, 20, or 30, the start time of your request is rounded down to the nearest time that
/// corresponds to even 5-, 10-, 20-, or 30-second divisions of a minute"), else the whole minute
/// its band rounds onto ("Start time less than 15 days ago - Round down to the nearest whole
/// minute", `GetMetricStatisticsInput$StartTime`; every range here is an hour old).
fn rounding_interval(period: f64) -> f64 {
    if [5.0, 10.0, 20.0, 30.0].contains(&period) {
        period
    } else {
        MINUTE
    }
}

/// The instant an observation stamped `at` is AGGREGATED at, which is what the window grid holds:
/// its own instant for the high-resolution series a sub-minute period needs, and the whole minute
/// it was published in for a regular-resolution one ("Setting this to 60 specifies this metric as
/// regular resolution, which CloudWatch stores at 1-minute resolution.",
/// `MetricDatum$StorageResolution`).
fn aggregated_at(at: f64, period: f64) -> f64 {
    if period < MINUTE {
        at
    } else {
        floored(at, MINUTE)
    }
}

/// Rule: a datapoint is stamped with the start of the window it aggregates, and the windows run in
/// `period`-long steps from the ROUNDED start — so the stamp is the rounded start plus a whole
/// number of periods, whatever instant the request started from.
#[tokio::test]
async fn every_answered_window_is_stamped_a_whole_number_of_periods_from_the_rounded_start() {
    let router = build_router();
    for (index, period) in PERIODS.iter().enumerate() {
        for (phase_index, phase) in START_PHASES.iter().enumerate() {
            let case = format!("a period of {period} seconds entered {phase} of the way in");
            let metric = format!("Stamped{index}x{phase_index}");
            // The observation sits in the middle of a window well inside the range, so the
            // window it lands in is neither the one the range opens in nor the one it closes in.
            let boundary = floored(now_secs() - AGE_SECS, *period);
            let at = boundary + period / 2.0;
            publish(&router, &metric, at, *period).await;
            let start = boundary - 2.0 * period + period * phase;
            let end = boundary + 3.0 * period;
            let anchor = floored(start, rounding_interval(*period));
            let expected_stamp = anchor + floored(aggregated_at(at, *period) - anchor, *period);
            let points = read_windows(&router, &case, &metric, *period, start, end).await;
            for (stamp, _) in &points {
                assert_eq!(
                    (stamp - anchor).rem_euclid(*period),
                    0.0,
                    "case {case:?}: a window of {period} seconds begins a whole number of \
                     periods after the rounded start {anchor}, got {stamp} from {points:?}"
                );
            }
            assert_eq!(
                points,
                vec![(expected_stamp, 1.0)],
                "case {case:?}: the observation at {at} belongs to the window at \
                 {expected_stamp} of the grid anchored at {anchor}, got {points:?}"
            );
        }
    }
}

/// Rule, read where it differs from a grid of clock multiples: the range opens at the ROUNDED
/// start, so an observation of a minute BEFORE that instant is not answered at all, while the
/// window that opens there answers its own — a read anchored on clock multiples of the period
/// would have reported both under one earlier stamp.
#[tokio::test]
async fn the_grid_opens_at_the_rounded_start_rather_than_on_a_clock_multiple_of_the_period() {
    let router = build_router();
    // Periods WIDER than the minute the start rounds onto, so the rounded start falls strictly
    // inside a clock window of the period and the two grids can be told apart.
    for (index, period) in [300.0_f64, 900.0, 3_600.0].iter().enumerate() {
        let case = format!("a period of {period} seconds opened two minutes past its boundary");
        let metric = format!("Anchored{index}");
        let boundary = floored(now_secs() - AGE_SECS, *period);
        // Two observations: one in the first minute of the clock window, one in the third.
        let before = boundary + 30.0;
        let after = boundary + 2.0 * MINUTE + 30.0;
        publish(&router, &metric, before, *period).await;
        publish(&router, &metric, after, *period).await;

        // A start two minutes into that window rounds to the whole minute, which is where the
        // grid opens: the observation of the first minute is outside the range.
        let start = boundary + 2.0 * MINUTE + 17.0;
        let anchor = boundary + 2.0 * MINUTE;
        let points = read_windows(&router, &case, &metric, *period, start, anchor + period).await;
        assert_eq!(
            points,
            vec![(anchor, 1.0)],
            "case {case:?}: expected the single observation of [{anchor}, {}) stamped at the \
             rounded start, got {points:?}",
            anchor + period
        );

        // The other observation is answered when the range opens at or before its own minute,
        // and the grid still opens at the rounded start.
        let case = format!("{case}, read from the boundary itself");
        let whole = read_windows(&router, &case, &metric, *period, boundary, boundary + period)
            .await;
        assert_eq!(
            whole,
            vec![(boundary, 2.0)],
            "case {case:?}: expected both observations in the window stamped {boundary}, got \
             {whole:?}"
        );
    }
}

/// Rule, read through the statistic itself: a series published once per minute answers one
/// observation per minute window, and a read at a WIDER period folds the minutes of each of its
/// windows into one — the windows being laid from the rounded start is what decides WHICH minutes
/// land together.
#[tokio::test]
async fn a_wider_read_folds_the_windows_it_covers() {
    let router = build_router();
    let metric = "FoldedMinutes";
    // Ten minutes of one observation per minute, each stamped inside its own minute, published
    // into a span that begins on a 5-minute boundary.
    let base = floored(now_secs() - AGE_SECS, 300.0);
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

    // Read at five minutes, from an instant in the MIDDLE of the third minute: the grid opens on
    // that whole minute, so the first window holds minutes 2..6 and the second the rest.
    let case = "the same ten minutes read five minutes at a time from the third minute";
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
        vec![(base + 2.0 * MINUTE, 5.0), (base + 7.0 * MINUTE, 3.0)],
        "case {case:?}: expected the minutes folded five to a window laid from the rounded start \
         {}, got {folded:?}",
        base + 2.0 * MINUTE
    );
}
