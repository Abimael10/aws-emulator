// SPDX-License-Identifier: CC0-1.0

//! The FORM every identity this service mints takes, over every write that mints one.
//!
//! One rule, asserted over a table whose rows differ along which write mints the identity and
//! along what the resource it names watches: an identity this service mints — the `CreationId` a
//! metric alarm read reports for the write that created it, and the `AnomalyDetectorId` a write
//! mints for an anomaly detection model ("The unique identifier of the anomaly detector that you
//! created or updated.", `PutAnomalyDetectorOutput$AnomalyDetectorId`, length 1..128, pattern
//! `^[A-Za-z0-9_./:%()+-]+$`) — is `<uuid>/<epoch-millis>`: the uuid of the write that minted it
//! and the instant that write happened at, in milliseconds. So no two of this service's minted
//! identities are shaped differently, and a client that parses one parses them all.
//!
//! The bytes of any one identity are the service's own per-run value; what every row here
//! asserts is the form, that the identity is the resource's own (unique, and the same one every
//! later read reports), and that the instant it names is the one the write happened at — never a
//! particular uuid or a particular millisecond.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    build_router, creation_identity_millis, described_alarm, metric_alarm_request, ok,
    reported_creation_id, AlarmKind,
};

/// The namespace the metrics of this suite live in.
const NAMESPACE: &str = "MyApp/MintedIdentity";

/// The epoch milliseconds the minted identity `identity` names, failing `case` when `identity` is
/// not of the one form a minted identity of this service takes.
///
/// Read through the harness's one reader of that form ([`creation_identity_millis`]), so this
/// suite and every suite that asserts an alarm's creation identity agree on what the form is.
fn minted_millis(identity: &str, case: &str) -> i64 {
    let millis = creation_identity_millis(identity).unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted identity of the form <uuid>/<epoch-millis>, got {identity:?}")
    });
    // The modelled identifier shape every minted identity has to stay inside:
    // `AnomalyDetectorId`, length 1..128, pattern `^[A-Za-z0-9_./:%()+-]+$`.
    assert!(
        !identity.is_empty()
            && identity.len() <= 128
            && identity
                .chars()
                .all(|glyph| glyph.is_ascii_alphanumeric() || "_./:%()+-".contains(glyph)),
        "case {case:?}: a minted identity stays inside the modelled identifier pattern, got \
         {identity:?}"
    );
    assert!(
        millis > 0,
        "case {case:?}: the identity names the instant the write happened at, got {millis} \
         milliseconds in {identity:?}"
    );
    millis
}

/// A single-metric write of `metric`, with the dimensions and statistic given.
fn single_metric(metric: &str, dimensions: Option<Value>, stat: &str) -> Value {
    let mut detector = json!({"Namespace": NAMESPACE, "MetricName": metric, "Stat": stat});
    if let Some(dimensions) = dimensions {
        detector["Dimensions"] = dimensions;
    }
    json!({"SingleMetricAnomalyDetector": detector})
}

/// A metric-math write over `metric`.
fn metric_math(metric: &str) -> Value {
    json!({"MetricMathAnomalyDetector": {"MetricDataQueries": [
        {
            "Id": "m1",
            "MetricStat": {
                "Metric": {"Namespace": NAMESPACE, "MetricName": metric},
                "Period": 300,
                "Stat": "Average",
            },
            "ReturnData": false,
        },
        {"Id": "ad1", "Expression": "m1 * 2", "ReturnData": true},
    ]}})
}

/// The identifier `PutAnomalyDetector` reported for `request`.
async fn put_detector(router: &MultiServiceRouter, request: &Value, case: &str) -> String {
    let body = ok(router, "PutAnomalyDetector", request).await;
    body.get("AnomalyDetectorId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an AnomalyDetectorId, got {body}"))
        .to_string()
}

/// Every model the store holds, with the identifier a read reports for it, keyed by the metric
/// the model watches so a row can find its own model among them.
async fn described_identifiers(router: &MultiServiceRouter) -> Vec<(Option<String>, String)> {
    let body = ok(
        router,
        "DescribeAnomalyDetectors",
        &json!({"AnomalyDetectorTypes": ["SINGLE_METRIC", "METRIC_MATH"]}),
    )
    .await;
    body.get("AnomalyDetectors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|detector| {
            (
                detector
                    .get("MetricName")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                detector
                    .get("AnomalyDetectorId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_write_mints_the_identifier_of_a_model_in_the_one_minted_form() {
    // Rows differ along the spelling that names the model and along what it watches — the
    // structure with dimensions, the structure without, the deprecated flat members, and the
    // metric-math structure — so the form is asserted over every write that mints a model's
    // identifier rather than over one recorded request.
    // The metric a read reports for the model, which is how a row finds its own model among
    // them: a metric-math model reports none ("Use SingleMetricAnomalyDetector.MetricName
    // property.", `AnomalyDetector$MetricName`), so that row looks for the model with no metric.
    let cases: Vec<(&str, Option<&str>, Value)> = vec![
        (
            "the single-metric structure with dimensions",
            Some("Latency"),
            single_metric(
                "Latency",
                Some(json!([{"Name": "Service", "Value": "checkout"}])),
                "Average",
            ),
        ),
        (
            "the single-metric structure with no dimensions and a percentile statistic",
            Some("Queued"),
            single_metric("Queued", None, "p99"),
        ),
        (
            "the deprecated flat metric members",
            Some("Errors"),
            json!({"Namespace": NAMESPACE, "MetricName": "Errors", "Stat": "Sum"}),
        ),
        (
            "the metric-math structure",
            None,
            metric_math("Throughput"),
        ),
    ];

    let router = build_router();
    let mut minted: Vec<(String, i64)> = Vec::new();
    for (case, metric, request) in cases {
        let identifier = put_detector(&router, &request, case).await;
        let millis = minted_millis(&identifier, case);

        // The identity is the model's OWN: no other model of the store carries it.
        assert!(
            minted.iter().all(|(other, _)| other != &identifier),
            "case {case:?}: every model carries an identifier of its own, got the repeated \
             {identifier:?} among {minted:?}"
        );
        // And the instant each one names is the instant of its own write, so a later write
        // never names an earlier instant than one already minted.
        if let Some((earlier, earlier_millis)) = minted.last() {
            assert!(
                millis >= *earlier_millis,
                "case {case:?}: the identity names the instant of its write, so {identifier:?} \
                 cannot predate {earlier:?}"
            );
        }
        minted.push((identifier.clone(), millis));

        // A READ of the model reports the identifier the write minted, unchanged: a client
        // enumerating models is handed the very identity it can delete or re-describe one with.
        let described = described_identifiers(&router).await;
        let reported = described
            .iter()
            .find(|(watched, _)| watched.as_deref() == metric)
            .map(|(_, reported)| reported.clone());
        assert_eq!(
            reported.as_deref(),
            Some(identifier.as_str()),
            "case {case:?}: a read reports the identifier the write minted, got {described:?}"
        );

        // A second write of the same definition UPDATES that model, so it reports the identity
        // the model was created with rather than minting a second one.
        let again = put_detector(&router, &request, case).await;
        assert_eq!(
            again, identifier,
            "case {case:?}: a write that updates a model reports the identity it was created \
             with"
        );
    }
}

#[tokio::test]
async fn the_model_an_alarm_band_creates_carries_the_same_minted_form() {
    // The other write that mints a model's identifier: the alarm whose threshold is drawn from a
    // band ("In an alarm based on an anomaly detection model, this is the ID of the
    // ANOMALY_DETECTION_BAND function used as the threshold for the alarm",
    // `PutMetricAlarmInput$ThresholdMetricId`). Rows differ along the band's width and along the
    // statistic and dimensions of the series it is drawn from.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a band of an explicit width over a dimensioned series",
            json!([
                {
                    "Id": "m1",
                    "ReturnData": true,
                    "MetricStat": {
                        "Metric": {
                            "Namespace": NAMESPACE,
                            "MetricName": "Latency",
                            "Dimensions": [{"Name": "Service", "Value": "checkout"}],
                        },
                        "Period": 60,
                        "Stat": "Average",
                    },
                },
                {"Id": "ad1", "Expression": "ANOMALY_DETECTION_BAND(m1, 2)", "ReturnData": true},
            ]),
        ),
        (
            "a band of the default width over a series with no dimensions",
            json!([
                {
                    "Id": "m1",
                    "ReturnData": true,
                    "MetricStat": {
                        "Metric": {
                            "Namespace": NAMESPACE,
                            "MetricName": "Errors",
                            "Dimensions": [],
                        },
                        "Period": 60,
                        "Stat": "Sum",
                    },
                },
                {"Id": "ad1", "Expression": "ANOMALY_DETECTION_BAND(m1)", "ReturnData": true},
            ]),
        ),
    ];

    for (case, queries) in rows {
        let router = build_router();
        ok(
            &router,
            "PutMetricAlarm",
            &json!({
                "AlarmName": "band-owner",
                "Metrics": queries,
                "ThresholdMetricId": "ad1",
                "ComparisonOperator": "LessThanLowerOrGreaterThanUpperThreshold",
                "EvaluationPeriods": 1,
                "TreatMissingData": "missing",
                "ActionsEnabled": false,
            }),
        )
        .await;

        let described = described_identifiers(&router).await;
        assert_eq!(
            described.len(),
            1,
            "case {case:?}: the alarm write creates the model its band is drawn from, got \
             {described:?}"
        );
        minted_millis(&described[0].1, case);
    }
}

#[tokio::test]
async fn both_minted_identities_of_one_store_share_the_one_form() {
    // The two resources of this service whose reads carry a minted identity, in one store: the
    // identifier of an anomaly detection model and the creation identity of a metric alarm. Both
    // go through one minting, so neither is shaped unlike the other.
    let router = build_router();
    let alarm_name = "minted-identity-alarm";
    ok(&router, "PutMetricAlarm", &metric_alarm_request(alarm_name)).await;
    let alarm = described_alarm(&router, AlarmKind::Metric, alarm_name)
        .await
        .unwrap_or(Value::Null);
    let creation_id = reported_creation_id(&alarm)
        .unwrap_or_else(|| panic!("expected a CreationId on the written alarm, got {alarm}"))
        .to_string();
    let creation_millis = minted_millis(&creation_id, "the creation identity of a metric alarm");

    let detector_id = put_detector(
        &router,
        &single_metric("Latency", None, "Average"),
        "the identifier of an anomaly detection model",
    )
    .await;
    let detector_millis = minted_millis(&detector_id, "the identifier of an anomaly detection model");

    assert_ne!(
        creation_id, detector_id,
        "each resource carries an identity of its own, got {creation_id:?} twice"
    );
    assert!(
        detector_millis >= creation_millis,
        "the model was written after the alarm, so its identity cannot name an earlier instant: \
         {detector_id:?} against {creation_id:?}"
    );
}
