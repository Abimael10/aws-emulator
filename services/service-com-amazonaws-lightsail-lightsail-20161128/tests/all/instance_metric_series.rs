// SPDX-License-Identifier: CC0-1.0

//! The SERIES `GetInstanceMetricData` answers with, driven over the wire through
//! the same front door a client uses.
//!
//! The rule under test: an instance publishes a metric from the moment the
//! service creates it, so a read over a window that covers the instants it has
//! been publishing in answers one datapoint per `period` window of them. Each
//! datapoint carries the metric's published `unit`, the start of its own window
//! as its `timestamp`, and exactly the `MetricStatistic` members the request
//! asked for — whose values hold together over the window's samples: the mean
//! lies between the extremes, the total is the mean over the sample count, and
//! every value lies in the domain the metric's own documentation gives it.
//!
//! A datapoint takes a moment to land — the first sampling instant of the
//! instance has to pass — which is why every case here POLLS for the series
//! instead of asserting on how fast the host it runs on is.
//!
//! The windows a series is cut into are a rule about time alone, and a window
//! wider than a test can wait for is unobservable over the wire; the unit tests
//! beside the state module assert that cutting over windows a request can name
//! but a test cannot live through.
//!
//! The harness, the published catalog and the request builder are the ones
//! `instance_metric_data.rs` already has.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::instance_lifecycle::Emulator;
use crate::instance_metric_data::{
    created_at_seconds, host, metric_data, request, MAX_PERIOD, MIN_PERIOD, PUBLISHED_UNITS,
};

/// How long a case waits for the series to carry its first datapoint.
///
/// Generous on purpose: the condition it waits for is the FIRST sampling instant
/// of the instance passing, which is a fraction of a second, so a deadline this
/// far out is only ever reached when no sample lands at all.
const SERIES_DEADLINE: Duration = Duration::from_secs(30);

/// How long between two polls of the series.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How far before an instance's creation a window starts, in seconds, so the
/// window covers everything the instance has published.
const WINDOW_LEAD: i64 = 2 * 60 * 60;

/// How far past an instance's creation a window ends, in seconds. Beyond any
/// instant this suite's own polling can reach, so the window stays open over the
/// whole of a case.
const WINDOW_TRAIL: i64 = 2 * 60 * 60;

/// The members of the modelled `MetricStatistic` enum, in the order a datapoint
/// carries them, each with the datapoint member it selects.
///
/// SOURCE: `smithy_query(query="shape",shape="MetricStatistic")` for the names,
/// and `smithy_query(query="shape",shape="MetricDatapoint")` for the members.
const STATISTIC_MEMBERS: &[(&str, &str)] = &[
    ("Minimum", "minimum"),
    ("Maximum", "maximum"),
    ("Sum", "sum"),
    ("Average", "average"),
    ("SampleCount", "sampleCount"),
];

/// The measurement every sample of every instance metric carries here.
///
/// This emulator runs no guest, so in every window the quantity each instance
/// metric measures is zero — and that is the value each metric's own
/// documentation gives such a window (the modelled
/// `GetInstanceMetricDataRequest$metricName`): a status check "can be either 0
/// (passed) or 1 (failed)" and nothing impairs this host, `MetadataNoToken` "is
/// 0" when nothing accesses the metadata service, each network and disk counter
/// is "the number of ... during the period" and nothing crossed an interface or
/// completed on the system disk, and `CPUUtilization` is "the percentage of
/// allocated compute units that are currently in use" with nothing using any.
const MEASURED: f64 = 0.0;

/// The granularity an instance's three status checks are published at, in
/// seconds, and the granularity of every other instance metric.
///
/// SOURCE: the modelled `GetInstanceMetricDataRequest$period` documentation:
/// "The StatusCheckFailed, StatusCheckFailed_Instance, and
/// StatusCheckFailed_System instance metric data is available in 1-minute (60
/// seconds) granularity. All other instance metric data is available in 5-minute
/// (300 seconds) granularity."
const STATUS_CHECK_GRANULARITY: i64 = 60;
/// See [`STATUS_CHECK_GRANULARITY`].
const METRIC_GRANULARITY: i64 = 300;

/// The three status checks, the metrics published at
/// [`STATUS_CHECK_GRANULARITY`].
const STATUS_CHECKS: &[&str] = &[
    "StatusCheckFailed",
    "StatusCheckFailed_Instance",
    "StatusCheckFailed_System",
];

/// The granularity `metric_name` is published at; see
/// [`STATUS_CHECK_GRANULARITY`].
fn granularity_of(metric_name: &str) -> i64 {
    if STATUS_CHECKS.contains(&metric_name) {
        STATUS_CHECK_GRANULARITY
    } else {
        METRIC_GRANULARITY
    }
}

/// The instance a case reads a series of: its name and the instant it was
/// created, which is where its series begins.
struct Host {
    name: &'static str,
    created: i64,
}

impl Host {
    /// Create the instance and answer the handle on it the cases below use.
    async fn create(emulator: &Emulator, name: &'static str) -> Self {
        let created = created_at_seconds(&host(emulator, name).await);
        Self { name, created }
    }
}

/// Poll the series for `metric_name` over a window covering everything `instance`
/// has published, answering its datapoints once it carries at least one.
async fn wait_for_series(
    emulator: &Emulator,
    instance: &Host,
    metric_name: &str,
    unit: &str,
    period: i64,
    statistics: &[&str],
) -> Vec<Value> {
    let case = format!("{metric_name} in {unit} at period {period} on {}", instance.name);
    let deadline = Instant::now() + SERIES_DEADLINE;
    loop {
        let body = emulator
            .ok(
                "GetInstanceMetricData",
                request(
                    instance.name,
                    metric_name,
                    unit,
                    period,
                    instance.created - WINDOW_LEAD,
                    instance.created + WINDOW_TRAIL,
                    statistics,
                ),
            )
            .await;
        let data = metric_data(&case, &body);
        if !data.is_empty() {
            return data;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the series to carry a datapoint within \
             {SERIES_DEADLINE:?}, got {body:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `f64` a datapoint member carries, or a panic naming `case`.
fn number(case: &str, datapoint: &Value, member: &str) -> f64 {
    datapoint[member].as_f64().unwrap_or_else(|| {
        panic!("case {case}: expected the member {member} on the datapoint, got {datapoint:?}")
    })
}

/// Assert everything the rule says about ONE datapoint of `metric_name`: its
/// unit, its window, the members it carries, and that those members hold
/// together and lie in the metric's own domain.
fn assert_datapoint(
    case: &str,
    datapoint: &Value,
    metric_name: &str,
    unit: &str,
    period: i64,
    created: i64,
    statistics: &[&str],
) {
    assert_eq!(
        datapoint["unit"].as_str(),
        Some(unit),
        "case {case}: expected the metric's published unit, got {datapoint:?}"
    );

    // "The timestamp": the start of the datapoint's own window, so it is a
    // multiple of the granularity, and it lies between the window the instance
    // was created in and the window this read happened in.
    let timestamp = number(case, datapoint, "timestamp") as i64;
    assert_eq!(
        timestamp.rem_euclid(period),
        0,
        "case {case}: expected a timestamp on a {period}-second window boundary, got {timestamp}"
    );
    assert!(
        timestamp >= created - created.rem_euclid(period) && timestamp <= created + WINDOW_TRAIL,
        "case {case}: expected a window of the instance's own lifetime, got {timestamp} for an \
         instance created at {created}"
    );

    // Each statistic the request named selects one member OF the datapoint, so
    // those are present and the rest are absent.
    for (statistic, member) in STATISTIC_MEMBERS {
        let asked = statistics.contains(statistic);
        assert_eq!(
            !datapoint[member].is_null(),
            asked,
            "case {case}: expected the member {member} to be present = {asked} for statistics \
             {statistics:?}, got {datapoint:?}"
        );
        if asked {
            let value = number(case, datapoint, member);
            assert!(
                value >= 0.0 && value.is_finite(),
                "case {case}: expected a finite, non-negative {member}, got {value}"
            );
        }
    }

    // A window in the answer holds at least the one sample that put it there,
    // and at most the samples the metric's own PUBLISHED granularity fits in a
    // window of this `period` — plus the instance's first sample, which lands
    // before its first whole granularity interval has passed.
    if statistics.contains(&"SampleCount") {
        let samples = number(case, datapoint, "sampleCount");
        let most = (period / granularity_of(metric_name) + 1) as f64;
        assert!(
            (1.0..=most).contains(&samples),
            "case {case}: expected between one and {most} samples in a {period}-second window \
             of a metric published every {} seconds, got {samples}",
            granularity_of(metric_name)
        );
    }

    // "Minimum - The lowest value observed during the specified period",
    // "Maximum - The highest": the mean of the window lies between them.
    if statistics.contains(&"Minimum")
        && statistics.contains(&"Maximum")
        && statistics.contains(&"Average")
    {
        let (minimum, maximum, average) = (
            number(case, datapoint, "minimum"),
            number(case, datapoint, "maximum"),
            number(case, datapoint, "average"),
        );
        assert!(
            minimum <= average && average <= maximum,
            "case {case}: expected minimum <= average <= maximum, got {minimum}, {average}, \
             {maximum}"
        );
    }

    // "Sum - All values submitted for the matching metric added together" and
    // "Average - The value of Sum / SampleCount": the total is the mean over the
    // count.
    if statistics.contains(&"Sum")
        && statistics.contains(&"Average")
        && statistics.contains(&"SampleCount")
    {
        let (sum, average, samples) = (
            number(case, datapoint, "sum"),
            number(case, datapoint, "average"),
            number(case, datapoint, "sampleCount"),
        );
        let expected = average * samples;
        assert!(
            (sum - expected).abs() <= expected.abs() * 1e-9 + 1e-9,
            "case {case}: expected sum = average * sampleCount = {expected}, got {sum}"
        );
    }

    // What every sample of every one of these metrics measured: nothing ran on
    // this guest, so each of them is [`MEASURED`] and the whole window is too.
    for (statistic, member) in STATISTIC_MEMBERS {
        if *statistic == "SampleCount" || !statistics.contains(statistic) {
            continue;
        }
        let value = number(case, datapoint, member);
        assert_eq!(
            value, MEASURED,
            "case {case}: no guest ran, so the {member} of the window is {MEASURED}, got {value}"
        );
    }
}

// ---------------------------------------------------------------------------
// Every published metric serves a series
// ---------------------------------------------------------------------------

/// RULE: every metric the catalog publishes serves datapoints over a window
/// covering what the instance has published, each carrying the metric's unit,
/// the statistics the request asked for, and values that hold together.
#[tokio::test]
async fn every_published_metric_serves_a_datapoint_for_a_window_it_has_published_in() {
    let emulator = Emulator::new();
    let instance = Host::create(&emulator, "series-host").await;

    // Every statistic, so one pass asserts the whole datapoint and every
    // invariant between its members.
    let statistics = &["Minimum", "Maximum", "Sum", "Average", "SampleCount"];
    for (metric_name, unit) in PUBLISHED_UNITS {
        let case = format!("{metric_name} in {unit}");
        let data = wait_for_series(
            &emulator,
            &instance,
            metric_name,
            unit,
            MIN_PERIOD,
            statistics,
        )
        .await;
        for datapoint in &data {
            assert_datapoint(
                &case,
                datapoint,
                metric_name,
                unit,
                MIN_PERIOD,
                instance.created,
                statistics,
            );
        }
    }
}

/// RULE: the granularity is the request's — a datapoint's window is a multiple
/// of the `period` it asked for, at every value of it the model admits.
#[tokio::test]
async fn the_requested_granularity_is_what_the_windows_are_cut_at() {
    let emulator = Emulator::new();
    let instance = Host::create(&emulator, "granularity-host").await;

    // Both published bounds of the modelled `MetricPeriod` range and two values
    // between them.
    for period in [MIN_PERIOD, 300, 3_600, MAX_PERIOD] {
        let case = format!("CPUUtilization at period {period}");
        let statistics = &["Average", "SampleCount"];
        let data = wait_for_series(
            &emulator,
            &instance,
            "CPUUtilization",
            "Percent",
            period,
            statistics,
        )
        .await;
        for datapoint in &data {
            assert_datapoint(
                &case,
                datapoint,
                "CPUUtilization",
                "Percent",
                period,
                instance.created,
                statistics,
            );
        }
    }
}

/// RULE: `statistics` selects which members a datapoint carries and nothing
/// else — the same window answered for different statistic sets carries exactly
/// those members, in any count and any order.
#[tokio::test]
async fn statistics_select_the_members_a_datapoint_carries() {
    let emulator = Emulator::new();
    let instance = Host::create(&emulator, "statistic-host").await;

    // Rows differing along the count, the members and the order.
    let sets: &[&[&str]] = &[
        &["Average"],
        &["Sum"],
        &["SampleCount"],
        &["Maximum", "Minimum"],
        &["Minimum", "Maximum", "Sum", "Average", "SampleCount"],
        &["SampleCount", "Average", "Sum", "Maximum", "Minimum"],
    ];
    for statistics in sets {
        let case = format!("NetworkIn for statistics {statistics:?}");
        let data =
            wait_for_series(&emulator, &instance, "NetworkIn", "Bytes", 300, statistics).await;
        for datapoint in &data {
            assert_datapoint(
                &case,
                datapoint,
                "NetworkIn",
                "Bytes",
                300,
                instance.created,
                statistics,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The series is the instance's own, and stays put
// ---------------------------------------------------------------------------

/// RULE: the series is DERIVED from the instance's own record, so reading it
/// twice reports the same measurements of the same window — only the count of
/// samples the window has accrued can grow, because the clock went on.
#[tokio::test]
async fn a_window_reports_the_same_measurements_on_every_read() {
    let emulator = Emulator::new();
    let instance = Host::create(&emulator, "stable-host").await;
    let statistics = &["Minimum", "Maximum", "Sum", "Average", "SampleCount"];

    for (metric_name, unit) in PUBLISHED_UNITS.iter().take(4) {
        let case = format!("{metric_name} read twice");
        let first = wait_for_series(
            &emulator,
            &instance,
            metric_name,
            unit,
            MAX_PERIOD,
            statistics,
        )
        .await;
        let second = wait_for_series(
            &emulator,
            &instance,
            metric_name,
            unit,
            MAX_PERIOD,
            statistics,
        )
        .await;
        assert_eq!(
            first.len(),
            second.len(),
            "case {case}: expected the same windows, got {second:?} after {first:?}"
        );
        for (before, after) in first.iter().zip(second.iter()) {
            assert_eq!(
                (before["timestamp"].as_f64(), before["average"].as_f64()),
                (after["timestamp"].as_f64(), after["average"].as_f64()),
                "case {case}: expected the same window to report the same average, got \
                 {after:?} after {before:?}"
            );
            assert!(
                number(&case, after, "sampleCount") >= number(&case, before, "sampleCount"),
                "case {case}: a window's sample count only grows, got {after:?} after \
                 {before:?}"
            );
        }
    }
}

/// RULE: a metric is published at the granularity its own documentation states
/// — one minute for an instance's three status checks, five minutes for every
/// other instance metric — so a window of the series holds no more samples than
/// that granularity fits in it, whatever `period` the request asked for. Read
/// moments after the create, each window holds the instance's first sample and
/// no more.
#[tokio::test]
async fn a_window_holds_the_samples_the_published_granularity_fits_in_it() {
    let emulator = Emulator::new();
    let instance = Host::create(&emulator, "granular-host").await;
    let statistics = &["SampleCount", "Average"];

    // Every published metric, so both documented granularities are covered,
    // crossed with the granularity the REQUEST asks for: a window narrower than
    // the metric's own publishing interval, one exactly it, and one far wider.
    for (metric_name, unit) in PUBLISHED_UNITS {
        for period in [MIN_PERIOD, 300, 3_600] {
            let case = format!("{metric_name} at period {period}");
            let data = wait_for_series(
                &emulator,
                &instance,
                metric_name,
                unit,
                period,
                statistics,
            )
            .await;
            let granularity = granularity_of(metric_name);
            for datapoint in &data {
                let samples = number(&case, datapoint, "sampleCount");
                let most = (period / granularity + 1) as f64;
                assert!(
                    (1.0..=most).contains(&samples),
                    "case {case}: a {period}-second window of a metric published every \
                     {granularity} seconds holds between one and {most} samples, got {samples}"
                );
                assert_eq!(
                    number(&case, datapoint, "average"),
                    MEASURED,
                    "case {case}: no guest ran, so every sample of the window measured \
                     {MEASURED}, got {datapoint:?}"
                );
            }
        }
    }
}

/// RULE: a series belongs to ONE instance: a window of one instance's series is
/// not answered for another, and deleting an instance takes its series with it.
#[tokio::test]
async fn a_series_belongs_to_the_instance_it_was_read_for() {
    let emulator = Emulator::new();
    let statistics = &["Average", "SampleCount"];
    let kept = Host::create(&emulator, "kept-host").await;
    let gone = Host::create(&emulator, "gone-host").await;

    // Both instances serve a series of their own.
    for instance in [&kept, &gone] {
        wait_for_series(
            &emulator,
            instance,
            "CPUUtilization",
            "Percent",
            MIN_PERIOD,
            statistics,
        )
        .await;
    }

    emulator
        .ok(
            "DeleteInstance",
            serde_json::json!({"instanceName": gone.name}),
        )
        .await;

    // The deleted instance has no series to read at all, and the one that is
    // still held still serves its own.
    let (status, refused) = emulator
        .call(
            "GetInstanceMetricData",
            request(
                gone.name,
                "CPUUtilization",
                "Percent",
                MIN_PERIOD,
                gone.created - WINDOW_LEAD,
                gone.created + WINDOW_TRAIL,
                statistics,
            ),
        )
        .await;
    assert_eq!(
        status, 400,
        "case reading the deleted instance's series: expected a 400 refusal, got {status} \
         {refused:?}"
    );
    let still_served = wait_for_series(
        &emulator,
        &kept,
        "CPUUtilization",
        "Percent",
        MIN_PERIOD,
        statistics,
    )
    .await;
    assert!(
        !still_served.is_empty(),
        "case the kept instance after the delete: expected its own series, got {still_served:?}"
    );
}
