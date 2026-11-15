// SPDX-License-Identifier: CC0-1.0

//! `GetInstanceMetricData` driven over the wire, through the same front door a
//! client uses: `POST /` with `X-Amz-Target: Lightsail_20161128.<Operation>` and
//! an `awsJson1_1` body, dispatched by the multi-service router, so the
//! modelled-constraint walk, the handler and the awsJson error envelope are all
//! exercised rather than bypassed.
//!
//! The rule under test here is the ENVELOPE of the read: for every metric the
//! catalog publishes, asked for in the unit that metric is published in, over
//! any window at any granularity the model admits, the answer is HTTP 200
//! carrying the requested `metricName` and the datapoints of the windows the
//! instance has been publishing in — so a window that lies outside that span
//! carries none; the call writes nothing a later caller can see; a metric asked
//! for in a unit it is not published in, a window whose end does not follow its
//! start, or a request naming no statistic is refused with the modelled 400
//! `InvalidInputException`; and a name no instance record answers to earns the
//! service's `DoesNotExist` envelope.
//!
//! What the datapoints of a window the instance HAS published in carry is the
//! series rule, which `instance_metric_series.rs` is about.
//!
//! Every window here is placed relative to the instance's own `createdAt` rather
//! than at a fixed instant, so a case means the same thing whatever the clock of
//! the host running it reads.
//!
//! The harness is the one `instance_lifecycle.rs` already has.

use serde_json::{json, Value};

use crate::instance_lifecycle::{error_code, Emulator};

/// The blueprint, bundle and zone every instance in this suite is created from:
/// the host's own configuration is not what any case here is about.
const BLUEPRINT: &str = "amazon_linux_2023";
const BUNDLE: &str = "micro_3_0";
const ZONE: &str = "us-east-1a";

/// Every instance metric and the unit its data is published in.
///
/// Written out here independently of the service's own catalog, so these rows
/// pin the published list rather than restating whatever the code holds.
///
/// SOURCE: the modelled `GetInstanceMetricDataRequest$metricName` documentation
/// ("Valid instance metric names are listed below, along with the most useful
/// `statistics` to include in your request, and the published `unit` value"),
/// which states one published unit per metric and covers every member of the
/// modelled `InstanceMetricName` enum.
pub(crate) const PUBLISHED_UNITS: &[(&str, &str)] = &[
    ("BurstCapacityPercentage", "Percent"),
    ("BurstCapacityTime", "Seconds"),
    ("CPUUtilization", "Percent"),
    ("NetworkIn", "Bytes"),
    ("NetworkOut", "Bytes"),
    ("StatusCheckFailed", "Count"),
    ("StatusCheckFailed_Instance", "Count"),
    ("StatusCheckFailed_System", "Count"),
    ("MetadataNoToken", "Count"),
    ("DiskReadOps", "Count"),
    ("DiskWriteOps", "Count"),
    ("DiskReadBytes", "Bytes"),
    ("DiskWriteBytes", "Bytes"),
    ("DiskTotalReadTime", "Seconds"),
    ("DiskTotalWriteTime", "Seconds"),
    ("DiskQueueLength", "Count"),
];

/// The granularities a request may ask for: both published bounds of the
/// modelled `MetricPeriod` range (60..=86400) and two values between them.
const PERIODS: &[i64] = &[MIN_PERIOD, 300, 3600, MAX_PERIOD];

/// The bounds of the modelled `MetricPeriod` range (`@range(min: 60, max:
/// 86400)` on `MetricPeriod`).
pub(crate) const MIN_PERIOD: i64 = 60;
/// See [`MIN_PERIOD`].
pub(crate) const MAX_PERIOD: i64 = 86_400;

/// Statistic sets a request may ask for, differing along the count, the members
/// and the order: one statistic, several, every member of the modelled
/// `MetricStatistic` enum, and the same set reversed.
const STATISTIC_SETS: &[&[&str]] = &[
    &["Average"],
    &["Sum"],
    &["Maximum", "Minimum"],
    &["Minimum", "Maximum", "Sum", "Average", "SampleCount"],
    &["SampleCount", "Average", "Sum", "Maximum", "Minimum"],
];

/// One day in seconds, the unit the windows of this suite are placed by.
const DAY: i64 = 24 * 60 * 60;

/// An instant far enough from the instance's creation for a window on that side
/// of it to be entirely outside what the instance has published: a year.
const FAR: i64 = 365 * DAY;

/// The body member `code` Lightsail sets on a rejection whose cause is that what
/// the request names does not exist.
const DOES_NOT_EXIST: &str = "DoesNotExist";

/// Create instance `name` and answer the record `GetInstance` reports for it, so
/// every case starts from a host the store really holds.
pub(crate) async fn host(emulator: &Emulator, name: &str) -> Value {
    emulator
        .ok(
            "CreateInstances",
            json!({
                "instanceNames": [name],
                "availabilityZone": ZONE,
                "blueprintId": BLUEPRINT,
                "bundleId": BUNDLE,
            }),
        )
        .await;
    emulator
        .ok("GetInstance", json!({"instanceName": name}))
        .await["instance"]
        .clone()
}

/// The instant `instance` was created, in whole epoch seconds — the anchor every
/// window in this suite is placed relative to.
pub(crate) fn created_at_seconds(instance: &Value) -> i64 {
    instance["createdAt"].as_f64().unwrap_or_else(|| {
        panic!("case reading the instance's creation instant: expected createdAt, got {instance:?}")
    }) as i64
}

/// One `GetInstanceMetricData` request body.
pub(crate) fn request(
    instance_name: &str,
    metric_name: &str,
    unit: &str,
    period: i64,
    start_time: i64,
    end_time: i64,
    statistics: &[&str],
) -> Value {
    json!({
        "instanceName": instance_name,
        "metricName": metric_name,
        "unit": unit,
        "period": period,
        "startTime": start_time,
        "endTime": end_time,
        "statistics": statistics,
    })
}

/// The datapoints an answer carries, or a panic naming `case`.
pub(crate) fn metric_data(case: &str, body: &Value) -> Vec<Value> {
    body["metricData"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a metricData array, got {body:?}"))
        .clone()
}

// ---------------------------------------------------------------------------
// The published catalog
// ---------------------------------------------------------------------------

/// RULE: every metric the catalog publishes, asked for in the unit it is
/// published in over a window at any granularity the model admits, is answered
/// with the requested `metricName` and the datapoints of the windows the
/// instance published in — of which a window that CLOSED before the instance was
/// created holds none, whichever metric and granularity the request names.
#[tokio::test]
async fn every_published_metric_answers_its_own_name_over_a_window_it_has_no_sample_for() {
    let emulator = Emulator::new();
    let instance = "metrics-host";
    let created = created_at_seconds(&host(&emulator, instance).await);

    for (index, (metric_name, unit)) in PUBLISHED_UNITS.iter().enumerate() {
        // The rows differ along the granularity, the statistic set and the size
        // and placement of the window as well as the metric. Every window here
        // ends before the instance existed, so no sampling instant of the
        // instance falls in it.
        let period = PERIODS[index % PERIODS.len()];
        let statistics = STATISTIC_SETS[index % STATISTIC_SETS.len()];
        let end_time = created - DAY - (index as i64) * 900;
        let start_time = end_time - period * (index as i64 + 1);
        let case = format!("{metric_name} in {unit} at period {period}");

        let (status, body) = emulator
            .call(
                "GetInstanceMetricData",
                request(
                    instance,
                    metric_name,
                    unit,
                    period,
                    start_time,
                    end_time,
                    statistics,
                ),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200, got {status} {body:?}"
        );
        assert_eq!(
            body["metricName"].as_str(),
            Some(*metric_name),
            "case {case}: expected the requested metric name back, got {body:?}"
        );
        let data = metric_data(&case, &body);
        assert!(
            data.is_empty(),
            "case {case}: the instance published nothing before it was created, so a window \
             that closed then must carry no datapoint, got {data:?}"
        );
    }
}

/// RULE: a window the instance holds no sample in carries no datapoint, wherever
/// it lies — before the instance was created, or ahead of any clock this
/// emulator reads.
#[tokio::test]
async fn a_window_outside_what_the_instance_published_carries_no_datapoint() {
    let emulator = Emulator::new();
    let instance = "window-host";
    let created = created_at_seconds(&host(&emulator, instance).await);

    // (case, startTime, endTime), both in epoch seconds.
    let windows: &[(&str, i64, i64)] = &[
        ("a window years before the instance existed", created - FAR, created - FAR + 3_600),
        ("a window that closes a day before the create", created - 2 * DAY, created - DAY),
        ("a window entirely in the future", created + FAR, created + FAR + 3_600),
        ("the shortest window the ordering admits, before the create", created - DAY, created - DAY + 1),
    ];

    for (case, start_time, end_time) in windows {
        let (status, body) = emulator
            .call(
                "GetInstanceMetricData",
                request(
                    instance,
                    "CPUUtilization",
                    "Percent",
                    MIN_PERIOD,
                    *start_time,
                    *end_time,
                    &["Average", "Maximum"],
                ),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200, got {status} {body:?}"
        );
        assert_eq!(
            body["metricName"].as_str(),
            Some("CPUUtilization"),
            "case {case}: expected the requested metric name back, got {body:?}"
        );
        let data = metric_data(case, &body);
        assert!(
            data.is_empty(),
            "case {case}: expected no datapoints, got {data:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Reading changes nothing
// ---------------------------------------------------------------------------

/// RULE: reading metrics reports; it does not change what a later caller sees.
/// The same request answers the same thing twice, and the instance record and
/// its state are exactly what they were before the reads.
#[tokio::test]
async fn reading_metrics_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    let instance = "quiet-host";
    let before = host(&emulator, instance).await;
    let created = created_at_seconds(&before);
    let state_before = emulator
        .ok("GetInstanceState", json!({"instanceName": instance}))
        .await;

    let mut answers = Vec::new();
    for round in 0..2 {
        for (metric_name, unit) in PUBLISHED_UNITS.iter().take(4) {
            let case = format!("round {round} of {metric_name}");
            let answer = emulator
                .ok(
                    "GetInstanceMetricData",
                    request(
                        instance,
                        metric_name,
                        unit,
                        300,
                        // A window closed before the create, so the answer is
                        // the same at any two instants this test reads at.
                        created - 2 * DAY,
                        created - DAY,
                        &["Average", "Sum"],
                    ),
                )
                .await;
            answers.push((case, answer));
        }
    }
    let repeated = answers.len() / 2;
    for index in 0..repeated {
        let (case, first) = &answers[index];
        let (_, second) = &answers[index + repeated];
        assert_eq!(
            first, second,
            "case {case}: a repeated read must answer the same thing, got {second:?}"
        );
    }

    let after = emulator
        .ok("GetInstance", json!({"instanceName": instance}))
        .await["instance"]
        .clone();
    assert_eq!(
        before, after,
        "case the instance record after the reads: expected {before:?}, got {after:?}"
    );
    let state_after = emulator
        .ok("GetInstanceState", json!({"instanceName": instance}))
        .await;
    assert_eq!(
        state_before, state_after,
        "case the instance state after the reads: expected {state_before:?}, got {state_after:?}"
    );
}

// ---------------------------------------------------------------------------
// Refusals the handler owns
// ---------------------------------------------------------------------------

/// RULE: a request the model admits but that cannot describe a series — a metric
/// asked for in a unit it is not published in, a window whose end does not
/// follow its start, or a request naming no statistic — is refused with the
/// modelled 400 `InvalidInputException`.
#[tokio::test]
async fn a_request_that_describes_no_series_is_refused_as_invalid_input() {
    let emulator = Emulator::new();
    let instance = "refusal-host";
    let before = host(&emulator, instance).await;
    let created = created_at_seconds(&before);
    let (start, end) = (created - 2 * DAY, created - DAY);

    // One row per way the request fails to describe a series, crossed over the
    // units a metric is NOT published in (a unit of the wrong dimension, and a
    // unit another metric of the same resource IS published in).
    let rows: &[(&str, Value)] = &[
        (
            "CPUUtilization asked for in Bytes",
            request(instance, "CPUUtilization", "Bytes", 300, start, end, &["Average"]),
        ),
        (
            "NetworkIn asked for in Percent",
            request(instance, "NetworkIn", "Percent", 300, start, end, &["Sum"]),
        ),
        (
            "DiskQueueLength asked for in Seconds",
            request(instance, "DiskQueueLength", "Seconds", 300, start, end, &["Average"]),
        ),
        (
            "StatusCheckFailed asked for in None",
            request(instance, "StatusCheckFailed", "None", MIN_PERIOD, start, end, &["Sum"]),
        ),
        (
            "BurstCapacityTime asked for in Percent",
            request(instance, "BurstCapacityTime", "Percent", 300, start, end, &["Maximum"]),
        ),
        (
            "a window that ends where it starts",
            request(instance, "CPUUtilization", "Percent", 300, start, start, &["Average"]),
        ),
        (
            "a window that ends before it starts",
            request(instance, "NetworkOut", "Bytes", MAX_PERIOD, end, start, &["Sum"]),
        ),
        (
            "a request naming no statistic",
            request(instance, "MetadataNoToken", "Count", 300, start, end, &[]),
        ),
    ];

    for (case, body) in rows {
        let (status, answered) = emulator
            .call("GetInstanceMetricData", body.clone())
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidInputException",
            "case {case}: expected the wire error InvalidInputException, got {answered:?}"
        );
        assert!(
            answered["message"].as_str().is_some_and(|m| !m.is_empty()),
            "case {case}: expected a message saying what was wrong, got {answered:?}"
        );
    }

    // A refused read is still a read: it changed nothing.
    let after = emulator
        .ok("GetInstance", json!({"instanceName": instance}))
        .await["instance"]
        .clone();
    assert_eq!(
        before, after,
        "case the instance record after every refusal: expected {before:?}, got {after:?}"
    );
}

/// RULE: a name no instance record answers to — never created, or created and
/// since deleted — earns the service's `DoesNotExist` envelope: HTTP 400, the
/// wire error `NotFoundException`, the body `code` `DoesNotExist`, and the
/// service's not-found message naming the requested identifier and the type it
/// was looked for among.
#[tokio::test]
async fn a_name_no_instance_answers_to_earns_the_does_not_exist_envelope() {
    let emulator = Emulator::new();

    // Names differing along what the rule generalises over — length, case,
    // digits and separators — plus one that existed and was deleted.
    let deleted = "short-lived";
    let created = created_at_seconds(&host(&emulator, deleted).await);
    emulator
        .ok("DeleteInstance", json!({"instanceName": deleted}))
        .await;

    let names = ["ab", "never-created", "Probe_7", "host-0123456789", deleted];
    for name in names {
        let case = format!("GetInstanceMetricData on {name:?}");
        let (status, body) = emulator
            .call(
                "GetInstanceMetricData",
                request(
                    name,
                    "CPUUtilization",
                    "Percent",
                    300,
                    created - 2 * DAY,
                    created - DAY,
                    &["Average"],
                ),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {case}: expected the wire error NotFoundException, got {body:?}"
        );
        assert_eq!(
            body["code"].as_str(),
            Some(DOES_NOT_EXIST),
            "case {case}: expected the body code {DOES_NOT_EXIST}, got {body:?}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(crate::instance_lifecycle::does_not_exist_message("Instance", name).as_str()),
            "case {case}: expected the service's not-found message, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals the modelled constraints own
// ---------------------------------------------------------------------------

/// RULE: a value outside a modelled constraint is refused by the boundary walk
/// with its own 400 `ValidationException`, before the handler runs — the
/// `MetricPeriod` range one past each published bound, and a name outside each
/// enum this input reaches.
#[tokio::test]
async fn a_value_outside_a_modelled_constraint_is_refused_by_the_boundary_walk() {
    let emulator = Emulator::new();
    let instance = "boundary-host";
    let created = created_at_seconds(&host(&emulator, instance).await);
    let (start, end) = (created - 2 * DAY, created - DAY);

    let rows: &[(&str, Value)] = &[
        (
            "a period one second below the range",
            request(instance, "CPUUtilization", "Percent", MIN_PERIOD - 1, start, end, &["Average"]),
        ),
        (
            "a period one second above the range",
            request(instance, "CPUUtilization", "Percent", MAX_PERIOD + 1, start, end, &["Average"]),
        ),
        (
            "a metric name outside the modelled enum",
            request(instance, "TeapotTemperature", "Percent", 300, start, end, &["Average"]),
        ),
        (
            "a unit outside the modelled enum",
            request(instance, "CPUUtilization", "Furlongs", 300, start, end, &["Average"]),
        ),
        (
            "a statistic outside the modelled enum",
            request(instance, "CPUUtilization", "Percent", 300, start, end, &["Median"]),
        ),
    ];

    for (case, body) in rows {
        let (status, answered) = emulator
            .call("GetInstanceMetricData", body.clone())
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case}: expected the boundary walk's ValidationException, got {answered:?}"
        );
    }

    // Both published bounds of the range are themselves accepted, and over a
    // window the instance has no sample in each answers the empty series.
    for period in [MIN_PERIOD, MAX_PERIOD] {
        let case = format!("the period bound {period}");
        let body = emulator
            .ok(
                "GetInstanceMetricData",
                request(
                    instance,
                    "CPUUtilization",
                    "Percent",
                    period,
                    created - FAR,
                    created - FAR + period,
                    &["Average"],
                ),
            )
            .await;
        assert!(
            metric_data(&case, &body).is_empty(),
            "case {case}: expected an empty series, got {body:?}"
        );
    }
}
