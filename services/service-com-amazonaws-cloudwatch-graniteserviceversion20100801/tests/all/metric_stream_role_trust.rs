// SPDX-License-Identifier: CC0-1.0

//! WHICH ROLE a metric stream can export under, over the TRUST DOCUMENTS the role carries — end
//! to end, with Amazon Data Firehose, Amazon S3 and the IAM emulator mounted beside this service
//! on one engine, so the document read is the one IAM itself stored and the records are asserted
//! where a client sees them.
//!
//! RULE: a metric stream exports under "the ARN of an IAM role that this metric stream will use
//! to access Amazon Kinesis Data Firehose resources" (`PutMetricStreamInput$RoleArn`), and taking
//! a role on has two halves — the role's trust policy has to admit the service principal making
//! the export, and the permissions in force on it have to allow the record write. So a stream
//! whose role IAM reports with a trust policy admitting NEITHER spelling of the principals
//! CloudWatch exports as publishes NOTHING, however complete that role's permissions are, while
//! its sibling on an admitting role exports normally. Either way the stream reports what it did
//! in its own `AWS/CloudWatch/MetricStreams` series under the `MetricStreamName` dimension: a
//! `PublishErrorRate` of 0 and a `MetricUpdate` for a stream that published, and a non-zero
//! `PublishErrorRate` with no `MetricUpdate` at all for one that could not.
//!
//! The rows differ along what the rule generalises over: which of the principals the document
//! names, which spelling of it (global, Regional), whether it names one or several, whether the
//! action is written with a wildcard, which effect the statement states, which Region's spelling
//! it names, and whether IAM holds the role at all. Every row's role carries the SAME inline
//! permissions, so the only thing a row varies is the trust document.
//!
//! The pipeline (CloudWatch, Firehose and Amazon S3 on one engine) and the clock-free way a case
//! reaches a past period are the ones `metric_stream_delivery` stands up; what this suite adds is
//! IAM beside them.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::metric_stream_delivery::{
    period_of, stamp, Emulator, ACCOUNT, PERIOD_SECS, POLL_INTERVAL, REGION,
};

/// The namespace a metric stream's own metrics live in, and the series this suite reads.
const METRIC_STREAMS_NAMESPACE: &str = "AWS/CloudWatch/MetricStreams";
pub(crate) const METRIC_UPDATE: &str = "MetricUpdate";
pub(crate) const PUBLISH_ERROR_RATE: &str = "PublishErrorRate";

/// The dimension naming which stream a metering series is about.
const STREAM_NAME_DIMENSION: &str = "MetricStreamName";

/// The namespace every row's stream selects, and the metric published into it.
const NAMESPACE: &str = "Custom/Trust";
const METRIC_NAME: &str = "Streamed";

/// The status a delivery stream takes records in (`DeliveryStreamStatus`).
const ACTIVE: &str = "ACTIVE";

/// How long a CONDITION is waited for before a case fails: far longer than the spans the
/// pipeline's own clocks need, so a loaded machine does not fail a case the clock would have
/// served. Nothing below waits a fixed time — every wait polls for the condition and ends as
/// soon as it holds.
pub(crate) const DEADLINE: Duration = Duration::from_secs(120);

/// How far back and ahead a metering read looks, in periods: enough to hold every observation a
/// case makes whichever minute the case straddles.
const READ_PERIODS_BACK: f64 = 10.0;
const READ_PERIODS_AHEAD: f64 = 2.0;

/// The role the Firehose destination itself writes its objects under: Firehose resolves every
/// `RoleARN` a stream's configuration names against IAM when the stream is written, so this role
/// has to exist, trust Firehose and be allowed the object write.
pub(crate) const DELIVERY_ROLE: &str = "firehose-delivery";

/// The permissions every metric-stream role in this suite carries: "This IAM role must include
/// the following permissions: firehose:PutRecord firehose:PutRecordBatch"
/// (`PutMetricStreamInput$RoleArn`). Identical on every row, so the trust document is the one
/// thing a row varies.
pub(crate) const EXPORT_PERMISSIONS: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["firehose:PutRecord","firehose:PutRecordBatch"],"Resource":"*"}]}"#
);

/// The permissions the delivery role carries: the object write a delivery makes.
pub(crate) const DELIVERY_PERMISSIONS: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:PutObject","#,
    r#""Resource":"*"}]}"#
);

/// A trust policy with one statement naming `services` (a JSON string or array), `action` and
/// `effect`, as a client would send it.
pub(crate) fn trust(effect: &str, action: &str, services: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"{effect}\",\
         \"Principal\":{{\"Service\":{services}}},\"Action\":\"{action}\"}}]}}"
    )
}

/// The role ARN of this account's role `name`.
pub(crate) fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Create the role `name` with `document` as its trust policy and `permissions` inline.
pub(crate) async fn given_role(
    emulator: &Emulator,
    case: &str,
    name: &str,
    document: &str,
    permissions: &str,
) {
    emulator
        .iam(
            case,
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", document.to_string()),
            ],
        )
        .await;
    emulator
        .iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "export".to_string()),
                ("PolicyDocument", permissions.to_string()),
            ],
        )
        .await;
}

/// The destination bucket and the delivery stream writing into it, under the delivery role.
pub(crate) async fn given_destination(emulator: &Emulator, bucket: &str, delivery_stream: &str) {
    emulator.create_bucket(bucket).await;
    emulator
        .firehose(
            "CreateDeliveryStream",
            &json!({
                "DeliveryStreamName": delivery_stream,
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": role_arn(DELIVERY_ROLE),
                    "BucketARN": format!("arn:aws:s3:::{bucket}"),
                    // The shortest wait `IntervalInSeconds` admits, so a case does not wait out
                    // a buffering interval it is not about.
                    "BufferingHints": {"SizeInMBs": 1, "IntervalInSeconds": 0},
                },
            }),
        )
        .await;
}

/// Wait until `delivery_stream` is `ACTIVE`, which is when it takes records.
pub(crate) async fn until_active(emulator: &Emulator, delivery_stream: &str) {
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

/// Publish one datapoint of the suite's metric, stamped one period back so the record it owes
/// covers a period that is already open.
async fn publish(emulator: &Emulator) {
    emulator
        .cloudwatch(
            "PutMetricData",
            &json!({
                "Namespace": NAMESPACE,
                "MetricData": [{
                    "MetricName": METRIC_NAME,
                    "Value": 7.0,
                    "Unit": "Count",
                    "Timestamp": stamp(1),
                }],
            }),
        )
        .await;
}

/// The `Sum` and the `Maximum` of one metering series of `stream` over the window around now,
/// and how many datapoints it holds — read through `GetMetricStatistics` exactly as a client
/// reads it.
pub(crate) async fn metered(
    emulator: &Emulator,
    metric_name: &str,
    stream: &str,
) -> (f64, f64, usize) {
    let answer = emulator
        .cloudwatch(
            "GetMetricStatistics",
            &json!({
                "Namespace": METRIC_STREAMS_NAMESPACE,
                "MetricName": metric_name,
                "Dimensions": [{"Name": STREAM_NAME_DIMENSION, "Value": stream}],
                "StartTime": period_of(0) - READ_PERIODS_BACK * PERIOD_SECS,
                "EndTime": period_of(0) + READ_PERIODS_AHEAD * PERIOD_SECS,
                "Period": PERIOD_SECS as i64,
                "Statistics": ["Sum", "Maximum"],
            }),
        )
        .await;
    let datapoints = answer["Datapoints"].as_array().cloned().unwrap_or_default();
    let sum = datapoints
        .iter()
        .filter_map(|datapoint| datapoint.get("Sum").and_then(Value::as_f64))
        .sum();
    let maximum = datapoints
        .iter()
        .filter_map(|datapoint| datapoint.get("Maximum").and_then(Value::as_f64))
        .fold(0.0_f64, f64::max);
    (sum, maximum, datapoints.len())
}

/// Wait until `stream` has reported an attempt at all — a `PublishErrorRate` datapoint — and
/// answer that series' `Maximum`.
///
/// The POSITIVE signal both legs produce: a stream that published reports a rate of 0 and one
/// that could not reports a non-zero rate, so neither leg of a row is decided by waiting a fixed
/// time for nothing to happen.
pub(crate) async fn reported_error_rate(emulator: &Emulator, case: &str, stream: &str) -> f64 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, maximum, datapoints) = metered(emulator, PUBLISH_ERROR_RATE, stream).await;
        if datapoints > 0 {
            return maximum;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {stream} must report a {PUBLISH_ERROR_RATE} datapoint for the publish \
             attempt it made, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `bucket` holds at least one object and answer its keys.
async fn exported_keys(emulator: &Emulator, case: &str, bucket: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let keys = emulator.keys(bucket).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected an exported object in {bucket}, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: the trust document IAM holds for this row's role (or none at all), and whether the
/// stream exports under it.
struct Row {
    /// What the row varies, named in every assertion it makes.
    case: &'static str,
    /// The trust document IAM holds for the row's role, or `None` for a role IAM holds nothing
    /// under.
    trust: Option<String>,
    /// Whether the stream's records reach the destination.
    exports: bool,
}

/// The table.
fn rows() -> Vec<Row> {
    let metric_streams = "streams.metrics.cloudwatch.amazonaws.com";
    let regional_metric_streams = format!("streams.metrics.cloudwatch.{REGION}.amazonaws.com");
    let regional_cloudwatch = format!("cloudwatch.{REGION}.amazonaws.com");
    vec![
        Row {
            case: "the metric-streams service principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                &format!("\"{metric_streams}\""),
            )),
            exports: true,
        },
        Row {
            case: "its Regional spelling",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                &format!("\"{regional_metric_streams}\""),
            )),
            exports: true,
        },
        Row {
            case: "the CloudWatch service principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                "\"cloudwatch.amazonaws.com\"",
            )),
            exports: true,
        },
        Row {
            case: "the Regional spelling of the CloudWatch service principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                &format!("\"{regional_cloudwatch}\""),
            )),
            exports: true,
        },
        Row {
            case: "the metric-streams principal in a list beside another service's",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                &format!("[\"ec2.amazonaws.com\",\"{metric_streams}\"]"),
            )),
            exports: true,
        },
        Row {
            case: "the metric-streams principal, the action written with a wildcard",
            trust: Some(trust("Allow", "sts:*", &format!("\"{metric_streams}\""))),
            exports: true,
        },
        Row {
            case: "another service's principal only",
            trust: Some(trust("Allow", "sts:AssumeRole", "\"ec2.amazonaws.com\"")),
            exports: false,
        },
        Row {
            case: "another Region's spelling of the metric-streams principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                "\"streams.metrics.cloudwatch.ap-south-1.amazonaws.com\"",
            )),
            exports: false,
        },
        Row {
            case: "the metric-streams principal, explicitly denied",
            trust: Some(trust(
                "Deny",
                "sts:AssumeRole",
                &format!("\"{metric_streams}\""),
            )),
            exports: false,
        },
        Row {
            case: "a statement about another action entirely",
            trust: Some(trust(
                "Allow",
                "s3:GetObject",
                &format!("\"{metric_streams}\""),
            )),
            exports: false,
        },
        Row {
            // A delivery role is routinely provisioned out of band, so a role this store holds
            // nothing under is no evidence about it: the export is made.
            case: "a role this account's IAM holds nothing under",
            trust: None,
            exports: true,
        },
    ]
}

/// RULE: only a role whose trust policy admits a principal CloudWatch exports as publishes
/// anything, and each stream's own series reports which of the two it was.
#[tokio::test]
async fn only_a_role_cloudwatch_can_assume_exports_through_its_destination() {
    let emulator = Emulator::with_iam();
    let rows = rows();

    // The role every destination writes under: Firehose resolves it when the stream is written,
    // so it exists before any delivery stream is created.
    given_role(
        &emulator,
        "the delivery role",
        DELIVERY_ROLE,
        &trust("Allow", "sts:AssumeRole", "\"firehose.amazonaws.com\""),
        DELIVERY_PERMISSIONS,
    )
    .await;

    // Every row's destination, role and metric stream is written first, so ONE publish drives
    // every stream and the suite waits for conditions rather than for a span per row.
    for (index, row) in rows.iter().enumerate() {
        let bucket = format!("trust-sink-{index}");
        let delivery_stream = format!("trust-stream-{index}");
        let role = format!("trust-role-{index}");
        given_destination(&emulator, &bucket, &delivery_stream).await;
        if let Some(document) = &row.trust {
            given_role(&emulator, row.case, &role, document, EXPORT_PERMISSIONS).await;
        }
    }
    for index in 0..rows.len() {
        until_active(&emulator, &format!("trust-stream-{index}")).await;
    }
    for (index, row) in rows.iter().enumerate() {
        // "PutMetricStream accepts a syntactically valid role": the write is served whatever the
        // role turns out to be, and the boundary is only visible in the data plane.
        emulator
            .cloudwatch(
                "PutMetricStream",
                &metric_stream_request(
                    &format!("trust-{index}"),
                    &format!("trust-stream-{index}"),
                    &role_arn(&format!("trust-role-{index}")),
                ),
            )
            .await;
        let read = emulator
            .cloudwatch("GetMetricStream", &json!({"Name": format!("trust-{index}")}))
            .await;
        assert_eq!(
            read["RoleArn"],
            json!(role_arn(&format!("trust-role-{index}"))),
            "case {:?}: the stream must report the role it was configured with, got {read}",
            row.case
        );
    }

    publish(&emulator).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let stream = format!("trust-{index}");
        let bucket = format!("trust-sink-{index}");
        let rate = reported_error_rate(&emulator, case, &stream).await;
        if row.exports {
            let keys = exported_keys(&emulator, case, &bucket).await;
            assert!(
                !keys.is_empty(),
                "case {case:?}: a stream whose role CloudWatch can assume must export, got \
                 {keys:?}"
            );
            assert_eq!(
                rate, 0.0,
                "case {case:?}: a stream that published everything it attempted must report a \
                 {PUBLISH_ERROR_RATE} of 0, got {rate}"
            );
            let (updates, _, _) = metered(&emulator, METRIC_UPDATE, &stream).await;
            assert!(
                updates >= 1.0,
                "case {case:?}: a stream that exported must count its updates, got \
                 {METRIC_UPDATE}={updates}"
            );
            continue;
        }
        assert!(
            rate > 0.0,
            "case {case:?}: a stream whose role CloudWatch cannot assume must report a non-zero \
             {PUBLISH_ERROR_RATE}, got {rate}"
        );
        let keys = emulator.keys(&bucket).await;
        assert!(
            keys.is_empty(),
            "case {case:?}: CloudWatch cannot assume that role, so nothing may be delivered \
             through it; {bucket} holds {keys:?}"
        );
        let (updates, _, datapoints) = metered(&emulator, METRIC_UPDATE, &stream).await;
        assert_eq!(
            (updates, datapoints),
            (0.0, 0),
            "case {case:?}: a stream that published nothing counts no update, got \
             {METRIC_UPDATE}={updates} over {datapoints} datapoint(s)"
        );
    }
}

/// RULE: a stopped stream attempts no publish at all, so it reports neither counter nor error
/// rate — while its running twin, driven by the same publish, reports both.
#[tokio::test]
async fn a_stopped_stream_reports_no_publish_attempt() {
    let emulator = Emulator::with_iam();
    let case = "a stopped stream beside a running twin";
    given_role(
        &emulator,
        case,
        DELIVERY_ROLE,
        &trust("Allow", "sts:AssumeRole", "\"firehose.amazonaws.com\""),
        DELIVERY_PERMISSIONS,
    )
    .await;
    given_destination(&emulator, "twin-sink", "twin-stream").await;
    given_role(
        &emulator,
        case,
        "twin-role",
        &trust(
            "Allow",
            "sts:AssumeRole",
            "\"streams.metrics.cloudwatch.amazonaws.com\"",
        ),
        EXPORT_PERMISSIONS,
    )
    .await;
    until_active(&emulator, "twin-stream").await;
    for stream in ["twin-running", "twin-stopped"] {
        emulator
            .cloudwatch(
                "PutMetricStream",
                &metric_stream_request(stream, "twin-stream", &role_arn("twin-role")),
            )
            .await;
    }
    emulator
        .cloudwatch("StopMetricStreams", &json!({"Names": ["twin-stopped"]}))
        .await;

    publish(&emulator).await;

    // The running twin's own reading is what says the publish's exports have settled: both
    // streams are driven by the same publish, so once this one has reported, the stopped one has
    // had its chance too.
    let rate = reported_error_rate(&emulator, case, "twin-running").await;
    assert_eq!(
        rate, 0.0,
        "case {case:?}: the running twin published everything it attempted, got {rate}"
    );
    for metric_name in [METRIC_UPDATE, PUBLISH_ERROR_RATE] {
        let (sum, _, datapoints) = metered(&emulator, metric_name, "twin-stopped").await;
        assert_eq!(
            (sum, datapoints),
            (0.0, 0),
            "case {case:?}: a stopped stream attempts no publish, so {metric_name} must hold no \
             datapoint, got a sum of {sum} over {datapoints}"
        );
    }

    // Started again, it reports what it attempts from then on — so the silence above is the
    // state, not a stream that can never report.
    emulator
        .cloudwatch("StartMetricStreams", &json!({"Names": ["twin-stopped"]}))
        .await;
    publish(&emulator).await;
    let restarted = reported_error_rate(&emulator, case, "twin-stopped").await;
    assert_eq!(
        restarted, 0.0,
        "case {case:?}: the started twin must report the publish it then made, got {restarted}"
    );
    let (updates, _, _) = metered(&emulator, METRIC_UPDATE, "twin-stopped").await;
    assert!(
        updates >= 1.0,
        "case {case:?}: the started twin must count the update it exported, got \
         {METRIC_UPDATE}={updates}"
    );
}

/// RULE: it is the DOCUMENT and not the stream that decides — repairing only the trust policy of
/// a role a stream already failed on lets that same stream export.
#[tokio::test]
async fn repairing_only_the_trust_policy_lets_the_same_stream_export() {
    let emulator = Emulator::with_iam();
    let case = "a role repaired after the stream failed on it";
    given_role(
        &emulator,
        case,
        DELIVERY_ROLE,
        &trust("Allow", "sts:AssumeRole", "\"firehose.amazonaws.com\""),
        DELIVERY_PERMISSIONS,
    )
    .await;
    given_destination(&emulator, "repair-sink", "repair-stream").await;
    given_role(
        &emulator,
        case,
        "repair-role",
        &trust("Allow", "sts:AssumeRole", "\"ec2.amazonaws.com\""),
        EXPORT_PERMISSIONS,
    )
    .await;
    until_active(&emulator, "repair-stream").await;
    emulator
        .cloudwatch(
            "PutMetricStream",
            &metric_stream_request("repaired", "repair-stream", &role_arn("repair-role")),
        )
        .await;

    publish(&emulator).await;
    let rate = reported_error_rate(&emulator, case, "repaired").await;
    assert!(
        rate > 0.0,
        "case {case:?}: the stream must report a failed publish before the repair, got {rate}"
    );
    let keys = emulator.keys("repair-sink").await;
    assert!(
        keys.is_empty(),
        "case {case:?}: nothing may be delivered before the repair, got {keys:?}"
    );

    // ONLY the trust policy changes — the stream is not rewritten.
    emulator
        .iam(
            case,
            "UpdateAssumeRolePolicy",
            &[
                ("RoleName", "repair-role".to_string()),
                (
                    "PolicyDocument",
                    trust(
                        "Allow",
                        "sts:AssumeRole",
                        "\"streams.metrics.cloudwatch.amazonaws.com\"",
                    ),
                ),
            ],
        )
        .await;
    publish(&emulator).await;
    let keys = exported_keys(&emulator, case, "repair-sink").await;
    assert!(
        !keys.is_empty(),
        "case {case:?}: the repaired role must export, got {keys:?}"
    );
    let (updates, _, _) = metered(&emulator, METRIC_UPDATE, "repaired").await;
    assert!(
        updates >= 1.0,
        "case {case:?}: the repaired stream must count the update it exported, got \
         {METRIC_UPDATE}={updates}"
    );
}
