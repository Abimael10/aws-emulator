// SPDX-License-Identifier: CC0-1.0

//! What identifies an anomaly detection model of this service, and who creates one.
//!
//! Three rules, each asserted over a table whose rows differ along what it generalises over:
//!
//! * **The identifier a read reports for a model is the model's OWN, minted for it by the write
//!   that created it** — not a function of what the model watches ("The unique identifier of the
//!   anomaly detector.", `AnomalyDetector$AnomalyDetectorId`, length 1..128, pattern
//!   `^[A-Za-z0-9_./:%()+-]+$`). So two stores that hold the same definition report two
//!   identifiers, a model deleted and created again reports a new one, and a second write of a
//!   definition the store already holds updates that model and reports the identifier it was
//!   created with.
//! * **What a read reports is the identity the write named, IN FULL**: the training
//!   configuration, present with the time ranges it excludes; every `Dimensions` list, a list of
//!   none where the write named none; and the account the series belongs to — "If you omit this
//!   parameter, the current account is used." (`SingleMetricAnomalyDetector$AccountId`) — so a
//!   `DescribeAnomalyDetectors` result fed straight back into `PutAnomalyDetector` names the
//!   same model rather than creating a second one.
//! * **The write that configures an alarm whose threshold is drawn from a band creates the model
//!   that band is drawn from**, where the account holds none: a band is a model's ("You can use
//!   the model to display a band of expected normal values when the metric is graphed.",
//!   `PutAnomalyDetector`) and such an alarm designates one as its threshold ("In an alarm based
//!   on an anomaly detection model, this is the ID of the ANOMALY_DETECTION_BAND function used as
//!   the threshold for the alarm", `PutMetricAlarmInput$ThresholdMetricId`) — so the model is
//!   there to read and to delete from the write onwards, while an alarm thresholded by a number
//!   creates none.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, call, ok};

/// The namespace every metric of this suite lives in.
const NAMESPACE: &str = "MyApp/DetectorIdentity";

/// Both modelled `AnomalyDetectorType` values, so one listing reports every model a row wrote:
/// "If empty, defaults to SINGLE_METRIC." (`DescribeAnomalyDetectorsInput$AnomalyDetectorTypes`)
fn every_type() -> Value {
    json!({"AnomalyDetectorTypes": ["SINGLE_METRIC", "METRIC_MATH"]})
}

/// The account a read names for a series whose write named none.
fn caller_account() -> String {
    engine::emulator_account_id().to_string()
}

/// Whether `id` is an identifier the model allows: `AnomalyDetectorId`, length 1..128, pattern
/// `^[A-Za-z0-9_./:%()+-]+$`.
fn is_modelled_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_./:%()+-".contains(character))
}

/// The identifier `PutAnomalyDetector` reported, requiring one the model allows.
async fn put_detector(router: &MultiServiceRouter, request: &Value, case: &str) -> String {
    let body = ok(router, "PutAnomalyDetector", request).await;
    let id = body
        .get("AnomalyDetectorId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an AnomalyDetectorId, got {body}"))
        .to_string();
    assert!(
        is_modelled_identifier(&id),
        "case {case:?}: expected an identifier inside the modelled pattern, got {id:?}"
    );
    id
}

/// Every model the store holds, as `DescribeAnomalyDetectors` reports them.
async fn described(router: &MultiServiceRouter) -> Vec<Value> {
    let body = ok(router, "DescribeAnomalyDetectors", &every_type()).await;
    body.get("AnomalyDetectors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The one model the store holds, for a row that wrote exactly one.
async fn the_one_model(router: &MultiServiceRouter, case: &str) -> Value {
    let listed = described(router).await;
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected exactly one model, got {listed:?}"
    );
    listed[0].clone()
}

/// A single-metric write of the metric `metric`, with the dimensions and statistic given.
fn single_metric(metric: &str, dimensions: Option<Value>, stat: &str) -> Value {
    let mut detector = json!({
        "Namespace": NAMESPACE,
        "MetricName": metric,
        "Stat": stat,
    });
    if let Some(dimensions) = dimensions {
        detector["Dimensions"] = dimensions;
    }
    json!({"SingleMetricAnomalyDetector": detector})
}

/// A metric-math write over the metric `metric`.
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

#[tokio::test]
async fn the_identifier_a_model_carries_is_minted_for_it_and_not_derived_from_it() {
    // Rows differ along WHICH spelling names the model and what it watches — the structure with
    // dimensions, the structure without, the deprecated flat members, and the metric-math
    // structure — so the rule is asserted over every form a write may name a model with.
    let cases: Vec<(&str, Value)> = vec![
        (
            "the single-metric structure with dimensions",
            single_metric(
                "Latency",
                Some(json!([{"Name": "Service", "Value": "checkout"}])),
                "Average",
            ),
        ),
        (
            "the single-metric structure with no dimensions and a percentile statistic",
            single_metric("Latency", None, "p99"),
        ),
        (
            "the deprecated flat metric members",
            json!({"Namespace": NAMESPACE, "MetricName": "Errors", "Stat": "Sum"}),
        ),
        ("the metric-math structure", metric_math("Throughput")),
    ];

    for (case, request) in cases {
        // Two separate stores holding the SAME definition: an identifier derived from what the
        // model watches would be the same in both, and a minted one is not.
        let router = build_router();
        let elsewhere = build_router();
        let id = put_detector(&router, &request, case).await;
        let other = put_detector(&elsewhere, &request, case).await;
        assert_ne!(
            id, other,
            "case {case:?}: an identifier is the model's own, so two stores holding the same \
             definition report two identifiers"
        );

        // A second write of the SAME definition updates that model, so it reports the
        // identifier the model was created with: "The unique identifier of the anomaly detector
        // that you created or updated." (`PutAnomalyDetectorOutput$AnomalyDetectorId`)
        let again = put_detector(&router, &request, case).await;
        assert_eq!(
            again, id,
            "case {case:?}: a second write of one definition updates that model"
        );
        let listed = described(&router).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: a second write of one definition adds no second model, got {listed:?}"
        );

        // The model deleted and written again is a DIFFERENT model, which is what lets a client
        // tell a re-created model from the one it replaced.
        ok(
            &router,
            "DeleteAnomalyDetector",
            &json!({"AnomalyDetectorId": id.clone()}),
        )
        .await;
        assert!(
            described(&router).await.is_empty(),
            "case {case:?}: the model deleted by its identifier must be gone"
        );
        let recreated = put_detector(&router, &request, case).await;
        assert_ne!(
            recreated, id,
            "case {case:?}: a model created again after a delete carries a new identifier"
        );
    }
}

#[tokio::test]
async fn two_models_of_one_store_carry_two_identifiers() {
    // The same metric under four identities — a statistic, a dimension, a namespace and the
    // metric name each telling one model from the next — so every coordinate of what a model
    // watches is one the store keys a separate model by.
    let router = build_router();
    let writes: Vec<(&str, Value)> = vec![
        ("the metric itself", single_metric("Latency", None, "Average")),
        (
            "another statistic over it",
            single_metric("Latency", None, "Sum"),
        ),
        (
            "the same statistic over a dimensioned series",
            single_metric(
                "Latency",
                Some(json!([{"Name": "Service", "Value": "checkout"}])),
                "Average",
            ),
        ),
        (
            "another metric of the namespace",
            single_metric("Errors", None, "Average"),
        ),
    ];
    let mut minted: Vec<String> = Vec::new();
    for (case, request) in &writes {
        let id = put_detector(&router, request, case).await;
        assert!(
            !minted.contains(&id),
            "case {case:?}: a model of its own carries an identifier of its own, got {id}"
        );
        minted.push(id);
    }
    let listed = described(&router).await;
    assert_eq!(
        listed.len(),
        writes.len(),
        "each definition is a model of its own, got {listed:?}"
    );
}

#[tokio::test]
async fn a_read_reports_the_identity_the_write_named_in_full() {
    // Rows differ along what the write left out — the dimensions, the account, the training
    // configuration, the excluded ranges inside it — and along the spelling that named the
    // model, which is what the reported identity has to be complete over.
    let cases: Vec<(&str, Value, Value, Value)> = vec![
        (
            "a write naming neither dimensions, account nor configuration",
            single_metric("Latency", None, "Average"),
            json!({
                "AccountId": caller_account(),
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [],
                "Stat": "Average",
            }),
            json!({"ExcludedTimeRanges": []}),
        ),
        (
            "a write naming dimensions but no configuration",
            single_metric(
                "Latency",
                Some(json!([{"Name": "Service", "Value": "checkout"}])),
                "p99",
            ),
            json!({
                "AccountId": caller_account(),
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [{"Name": "Service", "Value": "checkout"}],
                "Stat": "p99",
            }),
            json!({"ExcludedTimeRanges": []}),
        ),
        (
            "the deprecated flat members, with a configuration naming only a time zone",
            json!({
                "Namespace": NAMESPACE,
                "MetricName": "Queued",
                "Stat": "Maximum",
                "Configuration": {"MetricTimezone": "Europe/Dublin"},
            }),
            json!({
                "AccountId": caller_account(),
                "Namespace": NAMESPACE,
                "MetricName": "Queued",
                "Dimensions": [],
                "Stat": "Maximum",
            }),
            json!({"ExcludedTimeRanges": [], "MetricTimezone": "Europe/Dublin"}),
        ),
        (
            "a write naming the excluded ranges its model is trained around",
            json!({
                "SingleMetricAnomalyDetector": {
                    "Namespace": NAMESPACE,
                    "MetricName": "Throughput",
                    "Stat": "Sum",
                },
                "Configuration": {"ExcludedTimeRanges": [
                    {"StartTime": 1_700_000_000, "EndTime": 1_700_003_600},
                ]},
            }),
            json!({
                "AccountId": caller_account(),
                "Namespace": NAMESPACE,
                "MetricName": "Throughput",
                "Dimensions": [],
                "Stat": "Sum",
            }),
            json!({"ExcludedTimeRanges": [
                {"StartTime": 1_700_000_000, "EndTime": 1_700_003_600},
            ]}),
        ),
        (
            "a write naming an account of its own",
            json!({"SingleMetricAnomalyDetector": {
                "AccountId": "123456789012",
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Stat": "Average",
            }}),
            json!({
                "AccountId": "123456789012",
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [],
                "Stat": "Average",
            }),
            json!({"ExcludedTimeRanges": []}),
        ),
    ];

    for (case, request, expected_single, expected_configuration) in cases {
        let router = build_router();
        let id = put_detector(&router, &request, case).await;
        let detector = the_one_model(&router, case).await;
        assert_eq!(
            detector.get("AnomalyDetectorId").and_then(Value::as_str),
            Some(id.as_str()),
            "case {case:?}: the read reports the identifier the write minted, got {detector}"
        );
        assert_eq!(
            detector.get("SingleMetricAnomalyDetector").cloned(),
            Some(expected_single.clone()),
            "case {case:?}: expected the metric identity {expected_single}, got {detector}"
        );
        assert_eq!(
            detector.get("Configuration").cloned(),
            Some(expected_configuration.clone()),
            "case {case:?}: expected the training configuration {expected_configuration}, got \
             {detector}"
        );
        // The deprecated flat members mirror the structure, so a client reading either spelling
        // reads one model.
        for member in ["Namespace", "MetricName", "Dimensions", "Stat"] {
            assert_eq!(
                detector.get(member).cloned(),
                expected_single.get(member).cloned(),
                "case {case:?}: {member} must mirror the metric identity, got {detector}"
            );
        }

        // The read fed straight back in names the SAME model: the account it reports is the one
        // the series belongs to, so naming it changes nothing about which model is meant.
        let fed_back = json!({
            "SingleMetricAnomalyDetector": expected_single,
            "Configuration": expected_configuration,
        });
        let round_tripped = put_detector(&router, &fed_back, case).await;
        assert_eq!(
            round_tripped, id,
            "case {case:?}: a described model fed back into the write is that same model"
        );
        let listed = described(&router).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: feeding a described model back adds no second model, got {listed:?}"
        );
    }
}

/// One row of the band-alarm table: an alarm written with a `Metrics` array, and the model its
/// band is drawn from.
struct BandRow {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The alarm's query array.
    queries: Value,
    /// The query the alarm's `ThresholdMetricId` designates, `None` for a row thresholded by a
    /// number instead.
    threshold_metric_id: Option<&'static str>,
    /// The alarm's `ComparisonOperator`.
    comparison: &'static str,
    /// The static threshold a row thresholded by a number carries.
    threshold: Option<f64>,
    /// The metric identity the model this write must create watches, `None` for a row that must
    /// create none.
    expected_single: Option<Value>,
    /// How the row's own teardown names the model it deletes, `None` naming it by the identifier
    /// the read reported.
    delete_by: Option<Value>,
}

/// One metric query of a band alarm's array.
fn retrieval(id: &str, metric: &str, dimensions: Value, stat: &str) -> Value {
    json!({
        "Id": id,
        "ReturnData": id == "m1",
        "MetricStat": {
            "Metric": {
                "Namespace": NAMESPACE,
                "MetricName": metric,
                "Dimensions": dimensions,
            },
            "Period": 60,
            "Stat": stat,
        },
    })
}

#[tokio::test]
async fn the_write_that_names_a_band_creates_the_model_it_is_drawn_from() {
    // Rows differ along the band's width and the statistic and dimensions of the series it is
    // drawn from, along whether that series is the one the alarm GRADES, along how the teardown
    // names the model to delete, and — the last row — along whether the alarm names a band at
    // all.
    let rows: Vec<BandRow> = vec![
        BandRow {
            case: "a band of an explicit width over the graded series",
            queries: json!([
                retrieval("m1", "Latency", json!([{"Name": "Service", "Value": "checkout"}]), "Average"),
                {"Id": "ad1", "Expression": "ANOMALY_DETECTION_BAND(m1, 2)", "ReturnData": true},
            ]),
            threshold_metric_id: Some("ad1"),
            comparison: "GreaterThanUpperThreshold",
            threshold: None,
            expected_single: Some(json!({
                "AccountId": caller_account(),
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [{"Name": "Service", "Value": "checkout"}],
                "Stat": "Average",
            })),
            delete_by: None,
        },
        BandRow {
            case: "a band of the default width over a series with no dimensions",
            queries: json!([
                retrieval("m1", "Errors", json!([]), "Sum"),
                {"Id": "ad1", "Expression": "ANOMALY_DETECTION_BAND(m1)", "ReturnData": true},
            ]),
            threshold_metric_id: Some("ad1"),
            comparison: "LessThanLowerOrGreaterThanUpperThreshold",
            threshold: None,
            expected_single: Some(json!({
                "AccountId": caller_account(),
                "Namespace": NAMESPACE,
                "MetricName": "Errors",
                "Dimensions": [],
                "Stat": "Sum",
            })),
            // The identification the recorded teardown of such an alarm uses: the metric
            // definition the band reads, in the deprecated flat spelling.
            delete_by: Some(json!({
                "Namespace": NAMESPACE,
                "MetricName": "Errors",
                "Stat": "Sum",
            })),
        },
        BandRow {
            case: "a band drawn from a series the alarm does not grade",
            queries: json!([
                retrieval("m1", "Latency", json!([]), "Average"),
                retrieval("m2", "Queued", json!([{"Name": "Stage", "Value": "prod"}]), "Maximum"),
                {"Id": "ad1", "Expression": "ANOMALY_DETECTION_BAND(m2, 3)", "ReturnData": true},
            ]),
            threshold_metric_id: Some("ad1"),
            comparison: "LessThanLowerThreshold",
            threshold: None,
            expected_single: Some(json!({
                "AccountId": caller_account(),
                "Namespace": NAMESPACE,
                "MetricName": "Queued",
                "Dimensions": [{"Name": "Stage", "Value": "prod"}],
                "Stat": "Maximum",
            })),
            delete_by: Some(json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Queued",
                "Dimensions": [{"Name": "Stage", "Value": "prod"}],
                "Stat": "Maximum",
            }})),
        },
        BandRow {
            case: "an alarm thresholded by a number, which names no band",
            queries: json!([
                retrieval("m1", "Latency", json!([]), "Average"),
            ]),
            threshold_metric_id: None,
            comparison: "GreaterThanThreshold",
            threshold: Some(100.0),
            expected_single: None,
            delete_by: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = build_router();
        let mut request = json!({
            "AlarmName": "band-model-owner",
            "Metrics": row.queries,
            "ComparisonOperator": row.comparison,
            "EvaluationPeriods": 1,
            "TreatMissingData": "missing",
            "ActionsEnabled": false,
        });
        if let Some(designated) = row.threshold_metric_id {
            request["ThresholdMetricId"] = Value::from(designated);
        }
        if let Some(threshold) = row.threshold {
            request["Threshold"] = Value::from(threshold);
        }
        ok(&router, "PutMetricAlarm", &request).await;

        let Some(expected_single) = row.expected_single else {
            assert!(
                described(&router).await.is_empty(),
                "case {case:?}: an alarm that names no band creates no model"
            );
            continue;
        };

        // The model is there to READ from the write onwards: a caller inspecting the alarm's
        // threshold straight after configuring it sees the model it is drawn from, in the state
        // a model whose training has just started is in.
        let detector = the_one_model(&router, case).await;
        assert_eq!(
            detector.get("SingleMetricAnomalyDetector").cloned(),
            Some(expected_single.clone()),
            "case {case:?}: expected a model of {expected_single}, got {detector}"
        );
        assert_eq!(
            detector.get("StateValue").and_then(Value::as_str),
            Some("PENDING_TRAINING"),
            "case {case:?}: a model whose training has just started is pending training, got \
             {detector}"
        );
        assert_eq!(
            detector.get("Configuration").cloned(),
            Some(json!({"ExcludedTimeRanges": []})),
            "case {case:?}: a model the alarm write created is trained around no excluded \
             range, got {detector}"
        );
        let id = detector
            .get("AnomalyDetectorId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an identifier, got {detector}"))
            .to_string();
        assert!(
            is_modelled_identifier(&id),
            "case {case:?}: expected an identifier inside the modelled pattern, got {id:?}"
        );

        // And it is there to DELETE: the model the alarm write created is one the client can
        // remove, whether it names it by that identifier or by the metric the band reads.
        let delete_request = row
            .delete_by
            .unwrap_or_else(|| json!({"AnomalyDetectorId": id}));
        let (status, body) = call(&router, "DeleteAnomalyDetector", &delete_request).await;
        assert_eq!(
            (status, &body),
            (200, &json!({})),
            "case {case:?}: expected the model to be deleted with HTTP 200, got {status} with \
             {body}"
        );
    }
}
