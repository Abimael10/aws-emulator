// SPDX-License-Identifier: CC0-1.0

//! WHAT A CLIENT OBSERVES of a running metric stream's export, end to end over the wire:
//! CloudWatch, Amazon Data Firehose and Amazon S3 on one engine, exactly as the router mounts
//! them.
//!
//! The rule under test: a metric stream in the `running` state exports every metric update its
//! filters select into the delivery stream its `FirehoseArn` names — "Metric streams can
//! automatically stream CloudWatch metrics to Amazon Web Services destinations"
//! (`PutMetricStream`) — and METERS what it streamed in its own series, so one published datapoint
//! is observable twice and the two readings JOIN: the record that lands in the destination, named
//! the way the stream's `OutputFormat` names a metric, and one `MetricUpdate` of that stream in the
//! `AWS/CloudWatch/MetricStreams` namespace under the `MetricStreamName` dimension. For a stream
//! naming one of the OpenTelemetry releases (`PutMetricStreamInput$OutputFormat`: "Valid values are
//! `json`, `opentelemetry1.0`, and `opentelemetry0.7`") the record names its metric by the
//! qualified name of the series, since an OpenTelemetry metric carries ONE name where a CloudWatch
//! metric's identity is a namespace and a name, while the unqualified halves stay in the data
//! point's own identifying pairs; a stream naming `json` reports the two halves as members of its
//! own.
//!
//! Every assertion about a landed record is made on the bytes a consumer reads out of the
//! destination bucket, decoded with message declarations of this suite's own, and every reading of
//! a stream's own series is made through CloudWatch's `GetMetricStatistics` exactly as a client
//! makes it.
//!
//! The rows differ along what the rule generalises over: which release the stream names, how many
//! segments the namespace has, whether it is written with slashes or dots, and whether the metric
//! name itself carries a separator.
//!
//! The pipeline (CloudWatch, Firehose and Amazon S3 on one engine) and the clock-free way a case
//! reaches a past period are the ones `metric_stream_delivery` stands up; the expectation an
//! exported OpenTelemetry name is read against is `metric_stream_otel_export`'s.

use std::time::{Duration, Instant};

use prost::Message;
use serde_json::{json, Value};

use crate::metric_stream_delivery::{
    period_of, publish_datum, stamp, Emulator, ACCOUNT, PERIOD_SECS, POLL_INTERVAL, REGION,
    ROLE_ARN,
};
use crate::metric_stream_otel_export::qualified_name;

/// The `MetricStreamOutputFormat` values this suite exports under
/// (`smithy_query(query="shape",shape="MetricStreamOutputFormat")`).
const OPEN_TELEMETRY_1_0: &str = "opentelemetry1.0";
const OPEN_TELEMETRY_0_7: &str = "opentelemetry0.7";
const JSON: &str = "json";

/// The identifying pairs a data point carries the unqualified halves in.
const NAMESPACE_KEY: &str = "Namespace";
const METRIC_NAME_KEY: &str = "MetricName";

/// The namespace a stream's own metrics live in, and the series this suite joins a landed record
/// to.
const METRIC_STREAMS_NAMESPACE: &str = "AWS/CloudWatch/MetricStreams";
const METRIC_UPDATE: &str = "MetricUpdate";
const STREAM_NAME_DIMENSION: &str = "MetricStreamName";

/// The status a delivery stream takes records in (`DeliveryStreamStatus`).
const ACTIVE: &str = "ACTIVE";

/// How long a CONDITION is waited for before a case fails: far longer than the spans the
/// pipeline's own clocks need, so a loaded machine does not fail a case the clock would have
/// served. Nothing below waits a fixed time — every wait polls for its condition and ends as soon
/// as it holds.
const DEADLINE: Duration = Duration::from_secs(120);

/// How far back and ahead the metering read looks, in periods: enough to hold the observation a
/// case makes whichever minute it straddles.
const READ_PERIODS_BACK: f64 = 10.0;
const READ_PERIODS_AHEAD: f64 = 2.0;

// ---------------------------------------------------------------------------------------------
// The consumer's side of the protocol: the messages an exported record is decoded as here,
// declared from the OpenTelemetry metrics message names rather than from this emulator's
// renderer, so a record that is not those messages fails to decode.
// ---------------------------------------------------------------------------------------------

/// `opentelemetry.proto.collector.metrics.v1.ExportMetricsServiceRequest`.
#[derive(Clone, PartialEq, Message)]
struct ExportMetricsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    resource_metrics: Vec<ResourceMetrics>,
}

/// `opentelemetry.proto.metrics.v1.ResourceMetrics`, whose second member carries the metrics in
/// both releases (the scope of 1.0, the instrumentation library of 0.7).
#[derive(Clone, PartialEq, Message)]
struct ResourceMetrics {
    #[prost(message, repeated, tag = "2")]
    scope_metrics: Vec<ScopeMetrics>,
}

/// `opentelemetry.proto.metrics.v1.ScopeMetrics`.
#[derive(Clone, PartialEq, Message)]
struct ScopeMetrics {
    #[prost(message, repeated, tag = "2")]
    metrics: Vec<Metric>,
}

/// `opentelemetry.proto.metrics.v1.Metric`.
#[derive(Clone, PartialEq, Message)]
struct Metric {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(message, optional, tag = "11")]
    summary: Option<Summary>,
}

/// `opentelemetry.proto.metrics.v1.Summary`, whose data points are read as the bytes of the
/// release the stream names: the two releases differ inside the data point, and a
/// length-delimited member is the same bytes whether it is declared as a message or as that
/// message's encoding.
#[derive(Clone, PartialEq, Message)]
struct Summary {
    #[prost(bytes = "vec", repeated, tag = "1")]
    data_points: Vec<Vec<u8>>,
}

/// `opentelemetry.proto.metrics.v1.SummaryDataPoint` of the 1.0 release, whose identifying pairs
/// are attributes.
#[derive(Clone, PartialEq, Message)]
struct SummaryDataPoint {
    #[prost(message, repeated, tag = "7")]
    attributes: Vec<KeyValue>,
}

/// `opentelemetry.proto.metrics.v1.DoubleSummaryDataPoint` of the 0.7 release, whose identifying
/// pairs are string labels.
#[derive(Clone, PartialEq, Message)]
struct DoubleSummaryDataPoint {
    #[prost(message, repeated, tag = "1")]
    labels: Vec<StringKeyValue>,
}

/// `opentelemetry.proto.common.v1.KeyValue`.
#[derive(Clone, PartialEq, Message)]
struct KeyValue {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(message, optional, tag = "2")]
    value: Option<AnyValue>,
}

/// `opentelemetry.proto.common.v1.AnyValue`, of which the identifying pairs read here use the
/// string.
#[derive(Clone, PartialEq, Message)]
struct AnyValue {
    #[prost(string, optional, tag = "1")]
    string_value: Option<String>,
}

/// `opentelemetry.proto.common.v1.StringKeyValue`.
#[derive(Clone, PartialEq, Message)]
struct StringKeyValue {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(string, tag = "2")]
    value: String,
}

/// Every message the bytes of one delivered object carry, read one length-delimited message after
/// another until the bytes are spent.
fn messages_of(case: &str, key: &str, bytes: &[u8]) -> Vec<ExportMetricsServiceRequest> {
    let mut cursor: &[u8] = bytes;
    let mut messages = Vec::new();
    while !cursor.is_empty() {
        let message = ExportMetricsServiceRequest::decode_length_delimited(&mut cursor)
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: object {key} must be length-delimited OpenTelemetry metrics \
                     messages, got {error} over {} byte(s)",
                    bytes.len()
                )
            });
        messages.push(message);
    }
    messages
}

/// The metric one record reports: its name, and the identifying pairs its data point carries,
/// read through the declaration of the release the record is framed in.
struct Reported {
    name: String,
    pairs: Vec<(String, String)>,
}

/// Read one record's single metric and data point.
fn reported(case: &str, output_format: &str, message: &ExportMetricsServiceRequest) -> Reported {
    let metric = message
        .resource_metrics
        .first()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a record must carry one resource, got {message:?}")
        })
        .scope_metrics
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: a record must carry one scope, got {message:?}"))
        .metrics
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: a record must carry one metric, got {message:?}"))
        .clone();
    let encoded = metric
        .summary
        .as_ref()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a streamed metric must be summarised, got {metric:?}")
        })
        .data_points
        .first()
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a summary must carry one data point, got {metric:?}")
        });
    let pairs = if output_format == OPEN_TELEMETRY_1_0 {
        SummaryDataPoint::decode(encoded.as_slice())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the data point must be the 1.0 release's: {error}")
            })
            .attributes
            .iter()
            .map(|attribute| {
                (
                    attribute.key.clone(),
                    attribute
                        .value
                        .clone()
                        .and_then(|value| value.string_value)
                        .unwrap_or_default(),
                )
            })
            .collect()
    } else {
        DoubleSummaryDataPoint::decode(encoded.as_slice())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the data point must be the 0.7 release's: {error}")
            })
            .labels
            .iter()
            .map(|label| (label.key.clone(), label.value.clone()))
            .collect()
    };
    Reported {
        name: metric.name,
        pairs,
    }
}

/// The destination bucket and the Firehose delivery stream writing into it.
async fn given_destination(emulator: &Emulator, bucket: &str, delivery_stream: &str) {
    emulator.create_bucket(bucket).await;
    emulator
        .firehose(
            "CreateDeliveryStream",
            &json!({
                "DeliveryStreamName": delivery_stream,
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "BucketARN": format!("arn:aws:s3:::{bucket}"),
                    // The shortest wait `IntervalInSeconds` admits, so a case does not wait out a
                    // buffering interval it is not about.
                    "BufferingHints": {"SizeInMBs": 1, "IntervalInSeconds": 0},
                },
            }),
        )
        .await;
}

/// Wait until `delivery_stream` reports the status it takes records in, polling for the STATUS
/// rather than for a span of time.
async fn until_active(emulator: &Emulator, delivery_stream: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emulator
            .firehose(
                "DescribeDeliveryStream",
                &json!({"DeliveryStreamName": delivery_stream}),
            )
            .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == ACTIVE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "delivery stream {delivery_stream} must reach {ACTIVE} before it takes records, last \
             status {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `PutMetricStream` request for `name`, exporting only `namespace` in `output_format`.
fn metric_stream_request(
    name: &str,
    delivery_stream: &str,
    output_format: &str,
    namespace: &str,
) -> Value {
    json!({
        "Name": name,
        "FirehoseArn":
            format!("arn:aws:firehose:{REGION}:{ACCOUNT}:deliverystream/{delivery_stream}"),
        "RoleArn": ROLE_ARN,
        "OutputFormat": output_format,
        "IncludeFilters": [{"Namespace": namespace}],
    })
}

/// Publish one datapoint of `metric_name` into `namespace`, stamped one period back so the record
/// it owes covers a period that is already open.
async fn publish(emulator: &Emulator, namespace: &str, metric_name: &str) {
    publish_datum(
        emulator,
        namespace,
        json!({
            "MetricName": metric_name,
            "Value": 11.0,
            "Unit": "Count",
            "Timestamp": stamp(1),
        }),
    )
    .await;
}

/// Every record delivered into `bucket`, once at least one has arrived — or a failure naming
/// `case` once [`DEADLINE`] has passed.
async fn delivered_records(
    emulator: &Emulator,
    case: &str,
    bucket: &str,
) -> Vec<ExportMetricsServiceRequest> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut records: Vec<ExportMetricsServiceRequest> = Vec::new();
        for key in emulator.keys(bucket).await {
            let bytes = emulator.object(bucket, &key).await;
            records.extend(messages_of(case, &key, &bytes));
        }
        if !records.is_empty() {
            return records;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected an exported record in {bucket}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The text of every object `bucket` holds, once one has arrived — the json leg's records.
async fn delivered_text(emulator: &Emulator, case: &str, bucket: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut text = String::new();
        for key in emulator.keys(bucket).await {
            let bytes = emulator.object(bucket, &key).await;
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
        if !text.trim().is_empty() {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected an exported record in {bucket}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `Sum` of the `MetricUpdate` series of `stream`, read through `GetMetricStatistics` exactly
/// as a client reads it.
async fn metered_updates(emulator: &Emulator, stream: &str) -> f64 {
    let answer = emulator
        .cloudwatch(
            "GetMetricStatistics",
            &json!({
                "Namespace": METRIC_STREAMS_NAMESPACE,
                "MetricName": METRIC_UPDATE,
                "Dimensions": [{"Name": STREAM_NAME_DIMENSION, "Value": stream}],
                "StartTime": period_of(0) - READ_PERIODS_BACK * PERIOD_SECS,
                "EndTime": period_of(0) + READ_PERIODS_AHEAD * PERIOD_SECS,
                "Period": PERIOD_SECS as i64,
                "Statistics": ["Sum"],
            }),
        )
        .await;
    answer["Datapoints"]
        .as_array()
        .map(|datapoints| {
            datapoints
                .iter()
                .filter_map(|datapoint| datapoint.get("Sum").and_then(Value::as_f64))
                .sum()
        })
        .unwrap_or_default()
}

/// One naming case: the release the stream names, and the series it exports.
struct NameRow {
    case: &'static str,
    /// The `OutputFormat` the stream is created with.
    output_format: &'static str,
    /// The namespace the row publishes into, which its own stream streams and no other row's does.
    namespace: &'static str,
    metric_name: &'static str,
}

/// The table.
///
/// The rows differ along what the rule generalises over: which release the record is framed in,
/// how many segments the namespace has, whether it is written with slashes or dots, and whether
/// the metric name itself carries a separator — so the name is asserted as the composition of the
/// series' two halves rather than for one shape of input. No row publishes into the reserved
/// namespace space, which `PutMetricData` refuses.
fn rows() -> Vec<NameRow> {
    vec![
        NameRow {
            case: "the 1.0 release, a two-segment namespace",
            output_format: OPEN_TELEMETRY_1_0,
            namespace: "Custom/OtelNames",
            metric_name: "Latency",
        },
        NameRow {
            case: "the 1.0 release, a namespace of several segments",
            output_format: OPEN_TELEMETRY_1_0,
            namespace: "Custom/OtelNames/Deep/Nested",
            metric_name: "RequestCount",
        },
        NameRow {
            case: "the 1.0 release, a namespace written with dots",
            output_format: OPEN_TELEMETRY_1_0,
            namespace: "Custom.OtelNames.Dotted",
            metric_name: "Throughput",
        },
        NameRow {
            case: "the 1.0 release, a metric name carrying a separator of its own",
            output_format: OPEN_TELEMETRY_1_0,
            namespace: "Custom/OtelNames/Separated",
            metric_name: "DeliveryToS3/Records",
        },
        NameRow {
            case: "the 0.7 release, a two-segment namespace",
            output_format: OPEN_TELEMETRY_0_7,
            namespace: "Custom/OtelNames07",
            metric_name: "Errors",
        },
        NameRow {
            case: "the 0.7 release, a one-segment namespace",
            output_format: OPEN_TELEMETRY_0_7,
            namespace: "OtelNamesFlat",
            metric_name: "Invocations",
        },
    ]
}

/// The names of one row's resources, keyed by its index so no two rows share a destination.
fn bucket(index: usize) -> String {
    format!("otel-name-sink-{index}")
}
fn delivery_stream(index: usize) -> String {
    format!("otel-name-stream-{index}")
}
fn stream(index: usize) -> String {
    format!("otel-name-{index}")
}

/// RULE: a published datapoint lands in the destination as a record of the stream's own output
/// format, named the way that format names a metric, and the stream counts that same update in its
/// own `MetricUpdate` series.
#[tokio::test]
async fn a_published_datapoint_lands_as_a_record_the_stream_counts() {
    let emulator = Emulator::new();
    let rows = rows();

    // Every row's destination and stream is written first, so one publish per row drives them all
    // and the suite waits for conditions rather than for a span per row.
    for index in 0..rows.len() {
        given_destination(&emulator, &bucket(index), &delivery_stream(index)).await;
    }
    for index in 0..rows.len() {
        until_active(&emulator, &delivery_stream(index)).await;
    }
    for (index, row) in rows.iter().enumerate() {
        emulator
            .cloudwatch(
                "PutMetricStream",
                &metric_stream_request(
                    &stream(index),
                    &delivery_stream(index),
                    row.output_format,
                    row.namespace,
                ),
            )
            .await;
    }
    for row in &rows {
        publish(&emulator, row.namespace, row.metric_name).await;
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let records = delivered_records(&emulator, case, &bucket(index)).await;
        let read = reported(case, row.output_format, &records[0]);
        let expected = qualified_name(row.namespace, row.metric_name);
        assert_eq!(
            read.name, expected,
            "case {case:?}: expected the metric named {expected:?}, got {:?}",
            read.name
        );
        // The unqualified halves are still where a consumer reads them back.
        for (key, value) in [
            (NAMESPACE_KEY, row.namespace),
            (METRIC_NAME_KEY, row.metric_name),
        ] {
            let held = read
                .pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, held)| held.clone());
            assert_eq!(
                held.as_deref(),
                Some(value),
                "case {case:?}: expected the identifying pair {key} to be {value:?}, got {:?}",
                read.pairs
            );
        }
        // The landed record is an update the stream reports having streamed.
        let updates = metered_updates(&emulator, &stream(index)).await;
        assert!(
            updates >= 1.0,
            "case {case:?}: a stream that landed a record must count the update it streamed, got \
             {METRIC_UPDATE}={updates}"
        );
    }
}

/// RULE: the qualification belongs to the OpenTelemetry formats — a `json` stream fed by the same
/// publish reports the namespace and the metric name as members of its own, unqualified.
#[tokio::test]
async fn a_json_record_reports_the_two_halves_unqualified() {
    let namespace = "Custom/OtelNamesJson";
    let metric_name = "Shared";
    let case = "the json twin of an OpenTelemetry stream";
    let emulator = Emulator::new();
    let legs = [JSON, OPEN_TELEMETRY_1_0];
    for index in 0..legs.len() {
        given_destination(&emulator, &bucket(index), &delivery_stream(index)).await;
    }
    for index in 0..legs.len() {
        until_active(&emulator, &delivery_stream(index)).await;
    }
    for (index, format) in legs.into_iter().enumerate() {
        emulator
            .cloudwatch(
                "PutMetricStream",
                &metric_stream_request(&stream(index), &delivery_stream(index), format, namespace),
            )
            .await;
    }

    // ONE publish, selected by both streams.
    publish(&emulator, namespace, metric_name).await;

    let text = delivered_text(&emulator, case, &bucket(0)).await;
    let document: Value = serde_json::from_str(text.lines().next().unwrap_or_default())
        .unwrap_or_else(|error| {
            panic!("case {case:?}: a json record must be a JSON document: {error}")
        });
    assert_eq!(
        (
            document.get("namespace").and_then(Value::as_str),
            document.get("metric_name").and_then(Value::as_str)
        ),
        (Some(namespace), Some(metric_name)),
        "case {case:?}: a json record must report the two halves as its own members, got \
         {document}"
    );

    let records = delivered_records(&emulator, case, &bucket(1)).await;
    let read = reported(case, OPEN_TELEMETRY_1_0, &records[0]);
    let expected = qualified_name(namespace, metric_name);
    assert_eq!(
        read.name, expected,
        "case {case:?}: the OpenTelemetry twin of the same publish must name the metric \
         {expected:?}, got {:?}",
        read.name
    );
}
