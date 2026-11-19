// SPDX-License-Identifier: CC0-1.0

//! How a FLOATING-POINT member of this service's answers reaches the wire, in each of the
//! protocols its model declares.
//!
//! The rule under test: **a member this service's model types as a floating-point number is
//! rendered in the form the protocol of the REQUEST writes one in — as a decimal carrying a
//! fractional part in the XML text of an `awsQuery` answer (`95.0`, never `95`), and as a
//! NUMBER on the protocols whose codec has numbers of its own** — wherever the model puts such a
//! member: a statistic inside a reported datapoint, a threshold inside a reported alarm, a value
//! inside a map of them.
//!
//! The rows differ along what the rule generalises over: whether the value is whole, its sign,
//! its magnitude, how many fractional digits it already has, which member and which operation
//! carries it, and how deeply the model nests it (a member of a structure inside a list, and a
//! value inside a map). The one recorded case — a datapoint whose maximum is a whole number —
//! is one row among them.
//!
//! Member types from the model:
//! `smithy_query(query="shape",shape="Datapoint,MetricAlarm,Threshold,DatapointValue",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`
//! — `Datapoint`'s `SampleCount`/`Average`/`Sum`/`Minimum`/`Maximum` target `DatapointValue`
//! and `MetricAlarm$Threshold` targets `Threshold`, both `double`; `Datapoint$ExtendedStatistics`
//! is a map whose values are `DatapointValue` too.

use serde_json::{json, Value};

use crate::harness::{build_router, elements, merged, metric_alarm_request, ok, query_call};

/// The media type an `awsQuery` client sends its form body under, which is what names that
/// protocol for a request of this service.
const FORM: &str = "application/x-www-form-urlencoded";

/// The namespace and metric every datapoint row below publishes into.
const NAMESPACE: &str = "Custom/FloatRendering";
const METRIC: &str = "Latency";

/// The window every datapoint row aggregates over: one minute, the shortest period a regular
/// metric is read at.
const PERIOD: f64 = 60.0;

/// `secs` as the instant a raw query carries: "the time stamp must be in ISO 8601 UTC format"
/// (`GetMetricStatisticsInput$StartTime`).
fn iso(secs: f64) -> String {
    ::aws_smithy_types::DateTime::from_secs_f64(secs)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an epoch second renders as an ISO 8601 instant")
}

/// One row of the datapoint table below: the case, the observations it publishes into one
/// window, and the text each of the five statistics of the reported datapoint must render as.
/// Named so the row type stays simple enough to read.
type DatapointCase = (
    &'static str,
    &'static [f64],
    [(&'static str, &'static str); 5],
);

#[tokio::test]
async fn a_reported_threshold_renders_as_a_decimal_on_the_query_wire_and_a_number_on_the_json_one()
{
    // (case, the Threshold the alarm is written with, the text the XML wire must carry)
    let rows: [(&str, f64, &str); 8] = [
        ("a whole threshold", 5.0, "5.0"),
        ("the recorded observation as a threshold", 95.0, "95.0"),
        ("zero", 0.0, "0.0"),
        ("a negative whole value", -2.0, "-2.0"),
        ("a fraction below one", 0.5, "0.5"),
        ("a negative fraction", -0.25, "-0.25"),
        ("several fractional digits", 2048.25, "2048.25"),
        ("a large whole value", 1_000_000.0, "1000000.0"),
    ];
    for (case, threshold, expected) in rows {
        // A fresh emulator per row, so the alarm the row reads is the one it wrote.
        let router = build_router();
        let name = "float-rendering-alarm";
        ok(
            &router,
            "PutMetricAlarm",
            &merged(metric_alarm_request(name), &json!({"Threshold": threshold})),
        )
        .await;

        let (status, content_type, body) = query_call(
            &router,
            FORM,
            &[],
            "DescribeAlarms",
            &[("AlarmNames.member.1", name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the query call to be served, got {status} / {body}"
        );
        assert!(
            content_type.starts_with("text/xml"),
            "case {case:?}: expected the query answer in text/xml, got {content_type:?}"
        );
        let rendered = elements(&body, "Threshold");
        assert_eq!(
            rendered,
            vec![expected.to_string()],
            "case {case:?}: expected the threshold to render as {expected:?}, got {rendered:?} \
             in {body}"
        );

        // The same stored threshold on the JSON wire is a NUMBER: the decimal TEXT is the XML
        // protocol's form, not a change to the value the read reports.
        let described = ok(&router, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
        let on_json = described["MetricAlarms"][0]
            .get("Threshold")
            .cloned()
            .unwrap_or(Value::Null);
        assert!(
            on_json.is_number(),
            "case {case:?}: expected the JSON wire to carry a number, got {on_json}"
        );
        assert_eq!(
            on_json.as_f64(),
            Some(threshold),
            "case {case:?}: expected the JSON wire to carry {threshold}, got {on_json}"
        );
    }
}

#[tokio::test]
async fn every_statistic_of_a_reported_datapoint_renders_as_a_decimal_on_the_query_wire() {
    // (case, the observations published into one window, the statistics each must render as)
    // Rows differ along the values the window holds — whole, fractional, negative, a single
    // observation — so every statistic of the datapoint is a different number in each row.
    let rows: [DatapointCase; 4] = [
        (
            "two whole observations",
            &[90.0, 100.0],
            [
                ("SampleCount", "2.0"),
                ("Sum", "190.0"),
                ("Minimum", "90.0"),
                ("Maximum", "100.0"),
                ("Average", "95.0"),
            ],
        ),
        (
            "one observation, the recorded case's value",
            &[95.0],
            [
                ("SampleCount", "1.0"),
                ("Sum", "95.0"),
                ("Minimum", "95.0"),
                ("Maximum", "95.0"),
                ("Average", "95.0"),
            ],
        ),
        (
            "fractional observations",
            &[0.25, 0.75],
            [
                ("SampleCount", "2.0"),
                ("Sum", "1.0"),
                ("Minimum", "0.25"),
                ("Maximum", "0.75"),
                ("Average", "0.5"),
            ],
        ),
        (
            "observations either side of zero",
            &[-4.0, 0.0],
            [
                ("SampleCount", "2.0"),
                ("Sum", "-4.0"),
                ("Minimum", "-4.0"),
                ("Maximum", "0.0"),
                ("Average", "-2.0"),
            ],
        ),
    ];
    for (case, observations, expected) in rows {
        let router = build_router();
        // One window, so the read answers with exactly one datapoint: every observation is
        // stamped at the same instant, in the newest period that has already closed.
        let stamp = crate::harness::graded_window_stamp(PERIOD);
        let data: Vec<Value> = observations
            .iter()
            .map(|value| json!({"MetricName": METRIC, "Timestamp": stamp, "Value": value}))
            .collect();
        ok(
            &router,
            "PutMetricData",
            &json!({"Namespace": NAMESPACE, "MetricData": data}),
        )
        .await;

        let (status, _content_type, body) = query_call(
            &router,
            FORM,
            &[],
            "GetMetricStatistics",
            &[
                ("Namespace", NAMESPACE),
                ("MetricName", METRIC),
                ("StartTime", &iso(stamp - PERIOD)),
                ("EndTime", &iso(stamp + PERIOD)),
                ("Period", "60"),
                ("Statistics.member.1", "SampleCount"),
                ("Statistics.member.2", "Sum"),
                ("Statistics.member.3", "Minimum"),
                ("Statistics.member.4", "Maximum"),
                ("Statistics.member.5", "Average"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the query call to be served, got {status} / {body}"
        );
        for (member, text) in expected {
            let rendered = elements(&body, member);
            assert_eq!(
                rendered,
                vec![text.to_string()],
                "case {case:?}: expected {member} to render as {text:?}, got {rendered:?} in \
                 {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_double_inside_a_reported_map_renders_as_a_decimal_on_the_query_wire() {
    let router = build_router();
    let stamp = crate::harness::graded_window_stamp(PERIOD);
    // A window whose observations are all one whole number, so whichever percentile the read
    // computes over it is that number — and so the decimal form is the only thing the rendering
    // can differ in.
    ok(
        &router,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [
                {"MetricName": METRIC, "Timestamp": stamp, "Value": 95.0},
                {"MetricName": METRIC, "Timestamp": stamp, "Value": 95.0},
            ],
        }),
    )
    .await;

    let (status, _content_type, body) = query_call(
        &router,
        FORM,
        &[],
        "GetMetricStatistics",
        &[
            ("Namespace", NAMESPACE),
            ("MetricName", METRIC),
            ("StartTime", &iso(stamp - PERIOD)),
            ("EndTime", &iso(stamp + PERIOD)),
            ("Period", "60"),
            ("ExtendedStatistics.member.1", "p50"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the percentile read to be served, got {status} / {body}"
    );
    assert!(
        body.contains("p50"),
        "expected the answer to report the percentile it was asked for, got {body}"
    );
    let rendered = elements(&body, "value");
    assert_eq!(
        rendered,
        vec!["95.0".to_string()],
        "expected the percentile's value to render as \"95.0\", got {rendered:?} in {body}"
    );
}

#[tokio::test]
async fn the_json_wire_still_reports_a_datapoint_statistic_as_a_number() {
    let router = build_router();
    let stamp = crate::harness::graded_window_stamp(PERIOD);
    ok(
        &router,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [{"MetricName": METRIC, "Timestamp": stamp, "Value": 95.0}],
        }),
    )
    .await;
    let read = ok(
        &router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "StartTime": stamp - PERIOD,
            "EndTime": stamp + PERIOD,
            "Period": 60,
            "Statistics": ["Maximum"],
        }),
    )
    .await;
    let maximum = read["Datapoints"][0]
        .get("Maximum")
        .cloned()
        .unwrap_or(Value::Null);
    assert!(
        maximum.is_number(),
        "expected the JSON wire to carry the maximum as a number, got {maximum} in {read}"
    );
    assert_eq!(
        maximum.as_f64(),
        Some(95.0),
        "expected the JSON wire to carry 95, got {maximum}"
    );
}
