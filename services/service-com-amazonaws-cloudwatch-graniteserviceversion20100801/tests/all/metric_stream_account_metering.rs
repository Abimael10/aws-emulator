// SPDX-License-Identifier: CC0-1.0

//! What a client watching a METRIC STREAM reads about it without naming it: the ACCOUNT-level
//! aggregate of the series a stream publishes about its own export, beside the stream's own.
//!
//! RULE: every reading a running metric stream publishes about its export is published into TWO
//! series of the reserved `AWS/CloudWatch/MetricStreams` namespace — the stream's own, identified
//! by the `MetricStreamName` dimension naming it, and the account-level aggregate of the same
//! metric, which carries no dimension at all. A dimension set is part of a series' identity and a
//! read names it exactly ("If a specific combination of dimensions was not published, you can't
//! retrieve statistics for it", `GetMetricStatisticsInput$Dimensions`), so the aggregate is a
//! series of its own that every stream of the account meters into: its `Sum` over a window is
//! what the account's streams updated in that window — at least what any single stream did — and
//! the aggregated `PublishErrorRate` holds one observation per attempt every stream made, so a
//! stream whose role CloudWatch cannot assume raises the aggregate's `Maximum` above zero while
//! its healthy sibling keeps the aggregate's `Minimum` at zero.
//!
//! Only an end-to-end suite can ask this: the readings exist because records really reached (or
//! failed to reach) a destination, so CloudWatch, Amazon Data Firehose, Amazon S3 and IAM stand on
//! one engine exactly as the router mounts them, the trust document is the one IAM itself stored,
//! and every number is read back through CloudWatch's own `GetMetricStatistics`.
//!
//! The rows differ along what the rule generalises over: how many streams of the account are
//! metered, whether each of them can publish at all (its role's trust document), how many series
//! one publish touches, and how many periods those series fall in — so the aggregate is asserted
//! as a TOTAL over the account rather than for one stream's count.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::metric_stream_delivery::{period_of, stamp, Emulator, ACCOUNT, PERIOD_SECS, REGION};
use crate::metric_stream_role_trust::{
    given_destination, given_role, role_arn, trust, until_active, DELIVERY_PERMISSIONS,
    DELIVERY_ROLE, EXPORT_PERMISSIONS,
};

/// The namespace a metric stream's own metrics live in, and the three series this suite reads.
const METRIC_STREAMS_NAMESPACE: &str = "AWS/CloudWatch/MetricStreams";
const METRIC_UPDATE: &str = "MetricUpdate";
const TOTAL_METRIC_UPDATE: &str = "TotalMetricUpdate";
const PUBLISH_ERROR_RATE: &str = "PublishErrorRate";

/// The dimension naming which stream a metering series is about.
const STREAM_NAME_DIMENSION: &str = "MetricStreamName";

/// The namespace every row's streams select, and the metric published into it.
const NAMESPACE: &str = "Custom/Aggregate";
const METRIC_NAME: &str = "StreamedMetric";

/// The dimension the published series differ along, so one publish touches as many series as the
/// row names values for.
const SERIES_DIMENSION: &str = "Shard";

/// How long a delivered object is waited for before a case fails: far longer than the pipeline's
/// own buffering needs, so a loaded machine does not fail a case the clock would have served.
/// Nothing here waits a fixed time — the wait polls for the condition and ends as soon as it
/// holds.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How often the destination is polled while waiting for a delivery.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How far back and ahead a metering read looks, in periods: enough to hold every observation a
/// case makes whichever minute the case straddles, and far inside `GetMetricStatistics`' own
/// datapoint cap.
const READ_PERIODS_BACK: f64 = 10.0;
const READ_PERIODS_AHEAD: f64 = 2.0;

/// The trust document of a role CloudWatch can assume for an export: the metric-streams service
/// principal (`PutMetricStreamInput$RoleArn`, the role "this metric stream will use to access
/// Amazon Kinesis Data Firehose resources").
fn trusting_document() -> String {
    trust(
        "Allow",
        "sts:AssumeRole",
        "\"streams.metrics.cloudwatch.amazonaws.com\"",
    )
}

/// The trust document of a role it cannot: one naming another service's principal only.
fn untrusting_document() -> String {
    trust("Allow", "sts:AssumeRole", "\"ec2.amazonaws.com\"")
}

/// One stream of a row: whether CloudWatch can assume the role it exports under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publishes {
    /// Its role's trust policy admits the metric-streams principal, so its records arrive.
    Yes,
    /// Its role trusts another service only, so it attempts a publish and places nothing.
    No,
}

/// One row: the streams of the account, and what one publish into their namespace touches.
struct Row {
    /// What the row varies, named in every assertion it makes.
    case: &'static str,
    /// One entry per metric stream of the account, in creation order.
    streams: &'static [Publishes],
    /// The `Shard` values one publish carries, one series each.
    series: &'static [&'static str],
    /// The periods back those series are published into, one datapoint per (series, period).
    periods_back: &'static [i64],
}

impl Row {
    /// How many metric updates ONE publishing stream exports for this row: one record per series
    /// per period ("a period can be as short as one minute (60 seconds)", `MetricStat$Period`).
    fn updates_per_stream(&self) -> f64 {
        (self.series.len() * self.periods_back.len()) as f64
    }

    /// How many of the row's streams publish anything at all.
    fn publishing_streams(&self) -> f64 {
        self.streams
            .iter()
            .filter(|publishes| **publishes == Publishes::Yes)
            .count() as f64
    }
}

/// The table: the rows differ along the number of streams, whether each can publish, the number
/// of series one publish touches, and the number of periods they fall in.
const ROWS: &[Row] = &[
    Row {
        case: "one stream that can publish, three series in one period",
        streams: &[Publishes::Yes],
        series: &["one", "two", "three"],
        periods_back: &[1],
    },
    Row {
        case: "one stream that cannot publish",
        streams: &[Publishes::No],
        series: &["one", "two", "three"],
        periods_back: &[1],
    },
    Row {
        case: "a stream that can publish beside one that cannot",
        streams: &[Publishes::Yes, Publishes::No],
        series: &["one", "two", "three"],
        periods_back: &[1],
    },
    Row {
        case: "two streams that can both publish",
        streams: &[Publishes::Yes, Publishes::Yes],
        series: &["one", "two"],
        periods_back: &[1],
    },
    Row {
        case: "one stream, one series over three periods",
        streams: &[Publishes::Yes],
        series: &["one"],
        periods_back: &[1, 2, 3],
    },
];

/// What one metering series reports over the window around now, read through
/// `GetMetricStatistics` exactly as a client reads it.
#[derive(Debug, Clone, Copy)]
struct Reported {
    /// The `Sum` of every datapoint in the window.
    sum: f64,
    /// The greatest `Maximum` any of them reported, or `None` where the series holds none.
    maximum: Option<f64>,
    /// The smallest `Minimum` any of them reported, or `None` where the series holds none.
    minimum: Option<f64>,
    /// The observations those datapoints aggregated.
    sample_count: f64,
    /// How many datapoints the window held at all.
    datapoints: usize,
}

/// Read one metering series — `dimensions` names which, so the same read answers for a stream's
/// own series and for the account-level aggregate.
async fn reported(emulator: &Emulator, metric_name: &str, dimensions: Value) -> Reported {
    let answer = emulator
        .cloudwatch(
            "GetMetricStatistics",
            &json!({
                "Namespace": METRIC_STREAMS_NAMESPACE,
                "MetricName": metric_name,
                "Dimensions": dimensions,
                "StartTime": period_of(0) - READ_PERIODS_BACK * PERIOD_SECS,
                "EndTime": period_of(0) + READ_PERIODS_AHEAD * PERIOD_SECS,
                "Period": PERIOD_SECS as i64,
                "Statistics": ["Sum", "Maximum", "Minimum", "SampleCount"],
            }),
        )
        .await;
    let datapoints = answer
        .get("Datapoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let statistic = |name: &str| -> Vec<f64> {
        datapoints
            .iter()
            .filter_map(|datapoint| datapoint.get(name).and_then(Value::as_f64))
            .collect()
    };
    Reported {
        sum: statistic("Sum").into_iter().sum(),
        maximum: statistic("Maximum").into_iter().reduce(f64::max),
        minimum: statistic("Minimum").into_iter().reduce(f64::min),
        sample_count: statistic("SampleCount").into_iter().sum(),
        datapoints: datapoints.len(),
    }
}

/// The dimension list identifying the metering series of `stream`.
fn stream_dimension(stream: &str) -> Value {
    json!([{"Name": STREAM_NAME_DIMENSION, "Value": stream}])
}

/// The dimension list identifying the ACCOUNT-level aggregate: no dimension at all.
fn no_dimension() -> Value {
    json!([])
}

/// A `PutMetricStream` request for `name`, exporting `NAMESPACE` into `delivery_stream` under
/// `role`.
fn metric_stream_request(name: &str, delivery_stream: &str, role: &str) -> Value {
    json!({
        "Name": name,
        "FirehoseArn":
            format!("arn:aws:firehose:{REGION}:{ACCOUNT}:deliverystream/{delivery_stream}"),
        "RoleArn": role,
        "OutputFormat": "json",
        "IncludeFilters": [{"Namespace": NAMESPACE}],
    })
}

/// Publish one datapoint per (series, period) of `row`, in ONE call — so each running stream of
/// the account is owed one record for each of them.
async fn publish(emulator: &Emulator, row: &Row) {
    let data: Vec<Value> = row
        .series
        .iter()
        .flat_map(|shard| {
            row.periods_back.iter().map(move |back| {
                json!({
                    "MetricName": METRIC_NAME,
                    "Dimensions": [{"Name": SERIES_DIMENSION, "Value": shard}],
                    "Value": 5.0,
                    "Unit": "Count",
                    "Timestamp": stamp(*back),
                })
            })
        })
        .collect();
    emulator
        .cloudwatch(
            "PutMetricData",
            &json!({"Namespace": NAMESPACE, "MetricData": data}),
        )
        .await;
}

/// Wait until `bucket` holds at least one object and answer its keys.
async fn exported_keys(emulator: &Emulator, case: &str, bucket: &str) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let keys = emulator.keys(bucket).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivered object in {bucket}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Stand up one row's pipeline — the destination of each stream, the role it exports under, and
/// the stream itself — and publish into the namespace they select.
async fn given_row(emulator: &Emulator, row: &Row) {
    given_role(
        emulator,
        row.case,
        DELIVERY_ROLE,
        &trust("Allow", "sts:AssumeRole", "\"firehose.amazonaws.com\""),
        DELIVERY_PERMISSIONS,
    )
    .await;
    for (index, publishes) in row.streams.iter().enumerate() {
        given_destination(emulator, &bucket_of(index), &delivery_stream_of(index)).await;
        let document = match publishes {
            Publishes::Yes => trusting_document(),
            Publishes::No => untrusting_document(),
        };
        given_role(
            emulator,
            row.case,
            &export_role_of(index),
            &document,
            EXPORT_PERMISSIONS,
        )
        .await;
    }
    for index in 0..row.streams.len() {
        until_active(emulator, &delivery_stream_of(index)).await;
    }
    for index in 0..row.streams.len() {
        emulator
            .cloudwatch(
                "PutMetricStream",
                &metric_stream_request(
                    &stream_of(index),
                    &delivery_stream_of(index),
                    &role_arn(&export_role_of(index)),
                ),
            )
            .await;
    }
    publish(emulator, row).await;
}

/// The destination bucket of one row's `index`th stream.
fn bucket_of(index: usize) -> String {
    format!("aggregate-sink-{index}")
}

/// The delivery stream of one row's `index`th stream.
fn delivery_stream_of(index: usize) -> String {
    format!("aggregate-delivery-{index}")
}

/// The role one row's `index`th stream exports under.
fn export_role_of(index: usize) -> String {
    format!("aggregate-role-{index}")
}

/// The name of one row's `index`th metric stream.
fn stream_of(index: usize) -> String {
    format!("aggregate-stream-{index}")
}

/// RULE: each counter a stream publishes about its export is readable both under the dimension
/// naming that stream and, as the account's total, with no dimension at all.
#[tokio::test]
async fn the_account_aggregate_counts_what_every_stream_of_the_account_exported() {
    for row in ROWS {
        // Each row stands up its own emulator: the aggregate is the ACCOUNT's series, so two rows
        // sharing a store would meter into one another's aggregate.
        let emulator = Emulator::with_iam();
        given_row(&emulator, row).await;
        let case = row.case;
        let per_stream = row.updates_per_stream();

        for (index, publishes) in row.streams.iter().enumerate() {
            let stream = stream_of(index);
            let expected = match publishes {
                // "a metric stream always sends the MAX, MIN, SUM, and SAMPLECOUNT statistics for
                // each metric that is streamed" (`PutMetricStream`) and no row configures an
                // additional statistic, so the two counters agree on every row.
                Publishes::Yes => per_stream,
                Publishes::No => 0.0,
            };
            for metric_name in [METRIC_UPDATE, TOTAL_METRIC_UPDATE] {
                let own = reported(&emulator, metric_name, stream_dimension(&stream)).await;
                assert_eq!(
                    own.sum, expected,
                    "case {case:?}: {stream} must be metered for its own export, expected \
                     {metric_name}={expected}, got {own:?}"
                );
            }
        }

        // The aggregate is the account's TOTAL: every stream that exported counted into it, and a
        // stream that exported nothing counted nothing — so it is never below any one stream's
        // count and never a copy of it where two streams exported.
        let total = per_stream * row.publishing_streams();
        for metric_name in [METRIC_UPDATE, TOTAL_METRIC_UPDATE] {
            let aggregate = reported(&emulator, metric_name, no_dimension()).await;
            assert_eq!(
                aggregate.sum, total,
                "case {case:?}: the aggregate with no dimension must hold every stream's count, \
                 expected {metric_name}={total}, got {aggregate:?}"
            );
            // And so it is never below what any single stream of the account counted.
            assert!(
                row.publishing_streams() == 0.0 || aggregate.sum >= per_stream,
                "case {case:?}: the aggregate must be at least what one stream counted, expected \
                 {metric_name} of at least {per_stream}, got {aggregate:?}"
            );
        }
    }
}

/// RULE: the aggregated error rate holds one observation per attempt every stream made, so a
/// stream that could not publish shows up in it while its healthy sibling reports zero of its own.
#[tokio::test]
async fn the_aggregated_error_rate_holds_every_attempt_the_account_made() {
    for row in ROWS {
        let emulator = Emulator::with_iam();
        given_row(&emulator, row).await;
        let case = row.case;

        for (index, publishes) in row.streams.iter().enumerate() {
            let stream = stream_of(index);
            let own = reported(&emulator, PUBLISH_ERROR_RATE, stream_dimension(&stream)).await;
            assert!(
                own.datapoints > 0,
                "case {case:?}: {stream} attempted a publish, so it must report a \
                 {PUBLISH_ERROR_RATE} datapoint, got {own:?}"
            );
            // The rate is a SHARE, so every observation of it lies in [0, 1] whichever side of
            // the boundary a row's role is on.
            assert!(
                own.minimum.is_some_and(|value| value >= 0.0)
                    && own.maximum.is_some_and(|value| value <= 1.0),
                "case {case:?}: {stream} must report a share in [0, 1], got {own:?}"
            );
            match publishes {
                Publishes::Yes => assert_eq!(
                    (own.maximum, own.minimum),
                    (Some(0.0), Some(0.0)),
                    "case {case:?}: {stream} published everything it attempted, so every \
                     {PUBLISH_ERROR_RATE} observation must be exactly 0, got {own:?}"
                ),
                Publishes::No => assert!(
                    own.maximum.is_some_and(|value| value > 0.0),
                    "case {case:?}: CloudWatch cannot assume {stream}'s role, so it must report a \
                     non-zero {PUBLISH_ERROR_RATE}, got {own:?}"
                ),
            }
        }

        // One observation per attempt of the account: the aggregate holds as many as the row has
        // streams, bounded by the same [0, 1], and reports the worst and the best of them.
        let aggregate = reported(&emulator, PUBLISH_ERROR_RATE, no_dimension()).await;
        let attempts = row.streams.len() as f64;
        assert_eq!(
            aggregate.sample_count, attempts,
            "case {case:?}: the aggregate must hold one observation per attempt the account's \
             streams made, expected {attempts}, got {aggregate:?}"
        );
        assert!(
            aggregate.minimum.is_some_and(|value| value >= 0.0)
                && aggregate.maximum.is_some_and(|value| value <= 1.0),
            "case {case:?}: the aggregated share must lie in [0, 1], got {aggregate:?}"
        );
        assert_eq!(
            aggregate.maximum.is_some_and(|value| value > 0.0),
            row.streams.contains(&Publishes::No),
            "case {case:?}: the aggregate's Maximum must be above zero exactly when a stream of \
             the account could not publish, got {aggregate:?}"
        );
        assert_eq!(
            aggregate.minimum == Some(0.0),
            row.publishing_streams() > 0.0,
            "case {case:?}: the aggregate's Minimum must be zero exactly when a stream of the \
             account published everything it attempted, got {aggregate:?}"
        );
    }
}

/// RULE: the readings report a pipeline that really moved data — a stream whose role CloudWatch
/// can assume delivers its records to its destination while its untrusted sibling's prefix stays
/// empty, which is the differential the two series report.
#[tokio::test]
async fn the_readings_report_a_pipeline_that_really_delivered() {
    let row = &Row {
        case: "a trusted and an untrusted stream on one namespace",
        streams: &[Publishes::Yes, Publishes::No],
        series: &["one", "two", "three"],
        periods_back: &[1],
    };
    let emulator = Emulator::with_iam();
    given_row(&emulator, row).await;
    let case = row.case;

    let delivered = exported_keys(&emulator, case, &bucket_of(0)).await;
    assert!(
        !delivered.is_empty(),
        "case {case:?}: the stream whose role CloudWatch can assume must deliver, got {delivered:?}"
    );
    let withheld = emulator.keys(&bucket_of(1)).await;
    assert!(
        withheld.is_empty(),
        "case {case:?}: CloudWatch cannot assume the other role, so its destination must stay \
         empty, got {withheld:?}"
    );

    // What the aggregate reports is what the pipeline did: the delivered records are counted, and
    // the withheld attempt is the non-zero share.
    let counted = reported(&emulator, METRIC_UPDATE, no_dimension()).await;
    assert_eq!(
        counted.sum,
        row.updates_per_stream(),
        "case {case:?}: the aggregate must count the records that reached a destination, got \
         {counted:?}"
    );
    let rate = reported(&emulator, PUBLISH_ERROR_RATE, no_dimension()).await;
    assert!(
        rate.maximum.is_some_and(|value| value > 0.0) && rate.minimum == Some(0.0),
        "case {case:?}: the aggregated rate must hold both the failed and the clean attempt, got \
         {rate:?}"
    );
}
