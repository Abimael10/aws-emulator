// SPDX-License-Identifier: CC0-1.0

//! WHAT a metric stream carrying a `StatisticsConfigurations` exports, and WHETHER the role it
//! names lets it export at all — end to end, with Amazon Data Firehose, Amazon S3 and the IAM
//! emulator mounted beside this service on one engine, so the trust document read is the one IAM
//! itself stored and every record is asserted where a client sees it.
//!
//! RULE: a metric stream's export is a GATE and a PAYLOAD, and the two are independent. The gate:
//! a stream exports only while it is in the `running` state ("When you use `PutMetricStream` to
//! create a new metric stream, the stream is created in the `running` state", `PutMetricStream`)
//! and only when CloudWatch can take on "the ARN of an IAM role that this metric stream will use
//! to access Amazon Kinesis Data Firehose resources" (`PutMetricStreamInput$RoleArn`), which it
//! can only when that role's trust document admits a principal CloudWatch exports as. The
//! payload, once the gate opens: one record per selected metric per period, carrying the
//! statistics "a metric stream always sends … `MAX`, `MIN`, `SUM`, and `SAMPLECOUNT`" plus each
//! statistic the stream's `StatisticsConfigurations` adds for the metric that record is about
//! ("You can use the `StatisticsConfigurations` parameter to have the metric stream send
//! additional statistics in the stream", `PutMetricStream`), while the stream's own
//! `MetricUpdate` counts one per exported record and `TotalMetricUpdate` counts that record once
//! plus once per added statistic.
//!
//! So a stream whose gate stays shut delivers no object and counts no update however many
//! statistics its configuration names — a configuration never substitutes for a role CloudWatch
//! can assume — and a stream whose gate opens reports the full lift its configuration asks for,
//! whichever admitting document the role carries. Both halves are read back from the destination
//! bucket and from the stream's own `AWS/CloudWatch/MetricStreams` series.
//!
//! The pipeline (CloudWatch, Firehose and Amazon S3 on one engine), the clock-free way a case
//! reaches a past period and the IAM fixtures are the ones `metric_stream_delivery` and
//! `metric_stream_role_trust` stand up; what this suite adds is the two halves asserted together.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{json, Value};

use crate::metric_stream_delivery::{
    metric_of, publish_datum, stamp, statistic, Emulator, ACCOUNT, POLL_INTERVAL, REGION,
};
use crate::metric_stream_metering::{metering_of, METRIC_UPDATE, TOTAL_METRIC_UPDATE};
use crate::metric_stream_role_trust::{
    given_destination, given_role, metered, reported_error_rate, role_arn, trust, until_active,
    DEADLINE, DELIVERY_PERMISSIONS, DELIVERY_ROLE, EXPORT_PERMISSIONS, PUBLISH_ERROR_RATE,
};

/// The namespace every stream here selects, and the metrics published into it.
const NAMESPACE: &str = "Custom/StatisticsTrust";
const METRIC_NAME: &str = "Latency";
const OTHER_METRIC_NAME: &str = "Errors";

/// The distribution every case publishes, as `MetricDatum$Values` — several values, so a
/// percentile and a trimmed family are computed over a distribution rather than over one
/// observation.
const PUBLISHED: [f64; 10] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];

/// What the four statistics every streamed metric carries must report for [`PUBLISHED`], keyed as
/// the json record format names them.
const ALWAYS_SENT: [(&str, f64); 4] = [
    ("min", 1.0),
    ("max", 10.0),
    ("sum", 55.0),
    ("count", 10.0),
];

/// Publish [`PUBLISHED`] as one datum of `metric_name`, stamped one period back so the record it
/// owes covers a period that is already open.
async fn publish(emulator: &Emulator, metric_name: &str) {
    publish_datum(
        emulator,
        NAMESPACE,
        json!({
            "MetricName": metric_name,
            "Values": PUBLISHED,
            "Unit": "Count",
            "Timestamp": stamp(1),
        }),
    )
    .await;
}

/// A `PutMetricStream` request for `name`, selecting [`NAMESPACE`] into `delivery_stream` under
/// `role`, and asking for `statistics` on `on_metric` where a case names any.
fn metric_stream_request(
    name: &str,
    delivery_stream: &str,
    role: &str,
    on_metric: &str,
    statistics: &[(&str, f64)],
) -> Value {
    let mut request = json!({
        "Name": name,
        "FirehoseArn":
            format!("arn:aws:firehose:{REGION}:{ACCOUNT}:deliverystream/{delivery_stream}"),
        "RoleArn": role,
        "OutputFormat": "json",
        "IncludeFilters": [{"Namespace": NAMESPACE}],
    });
    if !statistics.is_empty() {
        let named: Vec<&str> = statistics.iter().map(|(name, _)| *name).collect();
        request["StatisticsConfigurations"] = json!([{
            "IncludeMetrics": [{"Namespace": NAMESPACE, "MetricName": on_metric}],
            "AdditionalStatistics": named,
        }]);
    }
    request
}

/// The metric name a record reports, which is the half of its identity a case selects on.
fn name_of(record: &Value) -> String {
    let (_, metric_name) = metric_of(record);
    metric_name
}

/// Wait until `bucket` holds a record for each of `want`, then answer every record it holds — or
/// fail naming `case` once [`DEADLINE`] has passed. What is waited for is the ARRIVAL of those
/// records, polled; nothing here waits a fixed span.
///
/// The records are read where a client reads them, through the destination's own objects
/// ([`Emulator::delivered_objects`]), and the condition is the METRICS rather than a count: a
/// destination can also hold records of a metric the case is not waiting for.
async fn records_for(emulator: &Emulator, case: &str, bucket: &str, want: &[&str]) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let records: Vec<Value> = emulator
            .delivered_objects(bucket)
            .await
            .into_iter()
            .flatten()
            .collect();
        let arrived: Vec<String> = records.iter().map(name_of).collect();
        if want
            .iter()
            .all(|metric_name| arrived.iter().any(|got| got == metric_name))
        {
            return records;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected an exported record for each of {want:?} in {bucket}, got \
             {records:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One case: the trust document IAM holds for this case's role, the state the stream is left in,
/// the additional statistics its configuration asks for with the value each must report over
/// [`PUBLISHED`], and whether the stream exports at all.
struct Row {
    /// What the row varies, named in every assertion it makes.
    case: &'static str,
    /// The trust document IAM holds for the row's role.
    trust: String,
    /// Whether the stream is left running, or stopped before anything is published.
    running: bool,
    /// The additional statistics the configuration asks for, each with the value it must report
    /// over [`PUBLISHED`].
    statistics: &'static [(&'static str, f64)],
    /// Whether the stream's records reach the destination.
    exports: bool,
}

/// The configured statistics a row asks for, with the value each must report over [`PUBLISHED`].
///
/// The names are the families `MetricStreamStatisticsConfiguration$AdditionalStatistics` points
/// at: "p?? percentile statistics such as p90, p99" and the abbreviated spans the json format also
/// admits ("this includes tm98, wm90"), of which `IQM` is the interquartile mean. The values are
/// the published distribution's: a percentile by nearest rank (`p50` is its fifth value, `p90` its
/// ninth, `p99` its largest) and the interquartile mean the mean of its middle half.
const ONE_PERCENTILE: [(&str, f64); 1] = [("p99", 10.0)];
const THREE_STATISTICS: [(&str, f64); 3] = [("p50", 5.0), ("p90", 9.0), ("IQM", 5.5)];
const NO_STATISTIC: [(&str, f64); 0] = [];

/// The table.
///
/// The rows differ along what the rule generalises over: how many additional statistics the
/// configuration asks for (none, one, three) and from which families, which document the role
/// carries (the metric-streams principal, its Regional spelling, another service's principal
/// only, that principal explicitly denied) and which state the stream is left in (running,
/// stopped) — so the gate and the lift are each asserted across the class rather than for one
/// shape of input.
fn rows() -> Vec<Row> {
    let metric_streams = "streams.metrics.cloudwatch.amazonaws.com";
    let regional = format!("streams.metrics.cloudwatch.{REGION}.amazonaws.com");
    let admitting = trust("Allow", "sts:AssumeRole", &format!("\"{metric_streams}\""));
    vec![
        Row {
            case: "no additional statistic, a role admitting the principal",
            trust: admitting.clone(),
            running: true,
            statistics: &NO_STATISTIC,
            exports: true,
        },
        Row {
            case: "one percentile, a role admitting the principal",
            trust: admitting.clone(),
            running: true,
            statistics: &ONE_PERCENTILE,
            exports: true,
        },
        Row {
            case: "three statistics, a role admitting the Regional spelling of the principal",
            trust: trust("Allow", "sts:AssumeRole", &format!("\"{regional}\"")),
            running: true,
            statistics: &THREE_STATISTICS,
            exports: true,
        },
        Row {
            case: "three statistics, a role trusting another service only",
            trust: trust("Allow", "sts:AssumeRole", "\"ec2.amazonaws.com\""),
            running: true,
            statistics: &THREE_STATISTICS,
            exports: false,
        },
        Row {
            case: "one percentile, a role denying the principal",
            trust: trust("Deny", "sts:AssumeRole", &format!("\"{metric_streams}\"")),
            running: true,
            statistics: &ONE_PERCENTILE,
            exports: false,
        },
        Row {
            case: "three statistics on a stopped stream",
            trust: admitting.clone(),
            running: false,
            statistics: &THREE_STATISTICS,
            exports: false,
        },
        Row {
            case: "no additional statistic on a stopped stream",
            trust: admitting,
            running: false,
            statistics: &NO_STATISTIC,
            exports: false,
        },
    ]
}

/// RULE: the additional statistics a stream is configured with reach its destination and lift its
/// `TotalMetricUpdate` exactly when CloudWatch can assume the role the stream names and the
/// stream is running — and a stream that cannot export delivers and counts nothing, whatever its
/// configuration asks for.
#[tokio::test]
async fn configured_statistics_are_exported_and_counted_only_under_an_assumable_role() {
    let emulator = Emulator::with_iam();
    let rows = rows();

    // The role every destination writes its objects under: Firehose resolves it when the delivery
    // stream is created, so it exists first.
    given_role(
        &emulator,
        "the delivery role",
        DELIVERY_ROLE,
        &trust("Allow", "sts:AssumeRole", "\"firehose.amazonaws.com\""),
        DELIVERY_PERMISSIONS,
    )
    .await;

    // Every row's destination, role and stream is written before anything is published, so ONE
    // publish drives every stream and the suite waits for conditions rather than for a span per
    // row. Every row's role carries the SAME permissions, so the only thing a row varies about it
    // is the trust document.
    for (index, row) in rows.iter().enumerate() {
        given_destination(
            &emulator,
            &format!("statistics-sink-{index}"),
            &format!("statistics-stream-{index}"),
        )
        .await;
        given_role(
            &emulator,
            row.case,
            &format!("statistics-role-{index}"),
            &row.trust,
            EXPORT_PERMISSIONS,
        )
        .await;
    }
    for index in 0..rows.len() {
        until_active(&emulator, &format!("statistics-stream-{index}")).await;
    }
    for (index, row) in rows.iter().enumerate() {
        let stream = format!("statistics-{index}");
        emulator
            .cloudwatch(
                "PutMetricStream",
                &metric_stream_request(
                    &stream,
                    &format!("statistics-stream-{index}"),
                    &role_arn(&format!("statistics-role-{index}")),
                    METRIC_NAME,
                    row.statistics,
                ),
            )
            .await;
        if !row.running {
            emulator
                .cloudwatch("StopMetricStreams", &json!({"Names": [stream]}))
                .await;
        }
    }

    publish(&emulator, METRIC_NAME).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let stream = format!("statistics-{index}");
        let bucket = format!("statistics-sink-{index}");
        if row.exports {
            let records = records_for(&emulator, case, &bucket, &[METRIC_NAME]).await;
            let record = records
                .iter()
                .find(|record| name_of(record) == METRIC_NAME)
                .unwrap_or_else(|| panic!("case {case:?}: the exported record must be there"));
            // The four every streamed metric carries, over the published distribution.
            for (name, expected) in ALWAYS_SENT {
                assert_eq!(
                    statistic(record, name),
                    Some(expected),
                    "case {case:?}: every record carries {name}={expected}, got {record}"
                );
            }
            // And each statistic the configuration added, at the value the distribution defines.
            for (name, expected) in row.statistics {
                assert_eq!(
                    statistic(record, name),
                    Some(*expected),
                    "case {case:?}: a configured statistic must be exported as {name}={expected}, \
                     got {record}"
                );
            }
            let rate = reported_error_rate(&emulator, case, &stream).await;
            assert_eq!(
                rate, 0.0,
                "case {case:?}: a stream that published everything it attempted must report a \
                 {PUBLISH_ERROR_RATE} of 0, got {rate}"
            );
            // One update exported, counted once in `MetricUpdate` and once plus once per added
            // statistic in the total.
            let (counted, total) = metering_of(&emulator, case, &stream, 1.0).await;
            let expected_total = 1.0 + row.statistics.len() as f64;
            assert_eq!(
                (counted, total),
                (1.0, expected_total),
                "case {case:?}: expected {METRIC_UPDATE}=1 and {TOTAL_METRIC_UPDATE}=\
                 {expected_total}, got {counted} and {total}"
            );
            continue;
        }

        // A stream that cannot export: the gate is shut, so neither the records nor the lift its
        // configuration asks for exist. A running stream on a role CloudWatch cannot assume
        // reports the attempt it made, which is the positive signal its leg settles on; a stopped
        // one attempts nothing at all, and the exporting rows above are what says the publish has
        // settled for every stream it drove.
        if row.running {
            let rate = reported_error_rate(&emulator, case, &stream).await;
            assert!(
                rate > 0.0,
                "case {case:?}: a stream whose role CloudWatch cannot assume must report a \
                 non-zero {PUBLISH_ERROR_RATE}, got {rate}"
            );
        } else {
            let (sum, _, datapoints) = metered(&emulator, PUBLISH_ERROR_RATE, &stream).await;
            assert_eq!(
                (sum, datapoints),
                (0.0, 0),
                "case {case:?}: a stopped stream attempts no publish, so it reports no \
                 {PUBLISH_ERROR_RATE}, got a sum of {sum} over {datapoints} datapoint(s)"
            );
        }
        let keys = emulator.keys(&bucket).await;
        assert!(
            keys.is_empty(),
            "case {case:?}: a stream that cannot export must deliver nothing however many \
             statistics it configures; {bucket} holds {keys:?}"
        );
        for metric_name in [METRIC_UPDATE, TOTAL_METRIC_UPDATE] {
            let (sum, _, datapoints) = metered(&emulator, metric_name, &stream).await;
            assert_eq!(
                (sum, datapoints),
                (0.0, 0),
                "case {case:?}: a stream that exported nothing counts no update, got \
                 {metric_name}={sum} over {datapoints} datapoint(s)"
            );
        }
    }
}

/// RULE: a configuration adds its statistics to the metrics it NAMES — so a stream selecting two
/// published metrics exports both, carries the additional statistics on the named one only, and
/// counts the lift once rather than per exported record.
#[tokio::test]
async fn the_lift_follows_the_metrics_the_configuration_names() {
    let emulator = Emulator::with_iam();
    let case = "one of two published metrics configured";
    given_role(
        &emulator,
        case,
        DELIVERY_ROLE,
        &trust("Allow", "sts:AssumeRole", "\"firehose.amazonaws.com\""),
        DELIVERY_PERMISSIONS,
    )
    .await;
    given_destination(&emulator, "named-sink", "named-stream").await;
    given_role(
        &emulator,
        case,
        "named-role",
        &trust(
            "Allow",
            "sts:AssumeRole",
            "\"streams.metrics.cloudwatch.amazonaws.com\"",
        ),
        EXPORT_PERMISSIONS,
    )
    .await;
    until_active(&emulator, "named-stream").await;
    emulator
        .cloudwatch(
            "PutMetricStream",
            &metric_stream_request(
                "named",
                "named-stream",
                &role_arn("named-role"),
                METRIC_NAME,
                &ONE_PERCENTILE,
            ),
        )
        .await;

    publish(&emulator, METRIC_NAME).await;
    publish(&emulator, OTHER_METRIC_NAME).await;

    let records = records_for(
        &emulator,
        case,
        "named-sink",
        &[METRIC_NAME, OTHER_METRIC_NAME],
    )
    .await;
    let by_metric: BTreeMap<String, &Value> = records
        .iter()
        .map(|record| (name_of(record), record))
        .collect();
    for (name, expected) in ONE_PERCENTILE {
        let named = by_metric
            .get(METRIC_NAME)
            .unwrap_or_else(|| panic!("case {case:?}: the configured metric must be exported"));
        assert_eq!(
            statistic(named, name),
            Some(expected),
            "case {case:?}: the configured metric must carry {name}={expected}, got {named}"
        );
        let other = by_metric
            .get(OTHER_METRIC_NAME)
            .unwrap_or_else(|| panic!("case {case:?}: the other metric must be exported too"));
        assert_eq!(
            statistic(other, name),
            None,
            "case {case:?}: a metric no configuration names must carry no additional statistic, \
             got {other}"
        );
    }

    // Two updates exported; only one of them carried the added statistic, so the total is three
    // rather than four.
    let (counted, total) = metering_of(&emulator, case, "named", 2.0).await;
    assert_eq!(
        (counted, total),
        (2.0, 3.0),
        "case {case:?}: expected {METRIC_UPDATE}=2 and {TOTAL_METRIC_UPDATE}=3, got {counted} \
         and {total}"
    );
}
