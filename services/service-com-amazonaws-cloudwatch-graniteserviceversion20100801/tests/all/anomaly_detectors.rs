// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the CloudWatch anomaly detection model loop: `PutAnomalyDetector`
//! creating and updating a model, `DescribeAnomalyDetectors` reading back what it persisted
//! (filtered and paged), and `DeleteAnomalyDetector` removing it.
//!
//! Every table row is a distinct input along what the rules under test generalise over — which
//! of the three spellings names the detector (the `SingleMetricAnomalyDetector` structure, the
//! `MetricMathAnomalyDetector` structure, or the deprecated flat metric members), which
//! statistic and how many dimensions the metric carries, what the training configuration
//! holds, which parameters are combined, which filter narrows a listing and how many models a
//! page holds — so the recorded happy path is one row among many rather than the
//! specification.

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, ok, wait_for_evaluation};

/// The namespace every detector in this suite watches a metric of, unless a row says
/// otherwise.
const NAMESPACE: &str = "MyApp/Anomalies";

/// The `AnomalyDetectorId` pattern the model declares (`AnomalyDetectorId`, length 1..128,
/// pattern `^[A-Za-z0-9_./:%()+-]+$`): every character an identifier this service mints may
/// carry.
fn is_modelled_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_./:%()+-".contains(character))
}

/// The identifier `PutAnomalyDetector` reported, requiring it to be one the model allows.
async fn put_detector(
    router: &smithy_server_runtime::MultiServiceRouter,
    request: &Value,
    case: &str,
) -> String {
    let body = ok(router, "PutAnomalyDetector", request).await;
    let id = body
        .get("AnomalyDetectorId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an AnomalyDetectorId, got {body}"))
        .to_string();
    assert!(
        is_modelled_identifier(&id),
        "case {case:?}: expected an AnomalyDetectorId inside the modelled pattern, got {id:?}"
    );
    id
}

/// The detector `DescribeAnomalyDetectors` reported under `id`, if any.
fn detector_with_id(body: &Value, id: &str) -> Option<Value> {
    body.get("AnomalyDetectors")?
        .as_array()?
        .iter()
        .find(|detector| detector.get("AnomalyDetectorId").and_then(Value::as_str) == Some(id))
        .cloned()
}

/// Every identifier a listing reported, in the order it reported them.
fn reported_ids(body: &Value) -> Vec<String> {
    body.get("AnomalyDetectors")
        .and_then(Value::as_array)
        .map(|detectors| {
            detectors
                .iter()
                .filter_map(|detector| detector.get("AnomalyDetectorId").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The message a refusal carries, under either spelling of the member the fault shapes of this
/// service declare it as.
fn fault_message(body: &Value) -> String {
    ["Message", "message"]
        .into_iter()
        .find_map(|member| body.get(member).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// A metric-math detector over the series the expression `expression` returns.
fn metric_math(metric: &str, expression: &str) -> Value {
    json!({"MetricDataQueries": [
        {
            "Id": "m1",
            "MetricStat": {
                "Metric": {"Namespace": NAMESPACE, "MetricName": metric},
                "Period": 300,
                "Stat": "Average",
            },
            "ReturnData": false,
        },
        {"Id": "ad1", "Expression": expression, "ReturnData": true},
    ]})
}

/// The single-metric structure a READ of a model reports, for a write that named `written`: the
/// identity in full — the account the series belongs to ("If you omit this parameter, the
/// current account is used.", `SingleMetricAnomalyDetector$AccountId`) and the dimensions it
/// carries, the EMPTY list where the write named none.
fn reported_single(written: &Value) -> Value {
    let mut reported = written.clone();
    if reported.get("AccountId").is_none() {
        reported["AccountId"] = Value::from(engine::emulator_account_id());
    }
    if reported.get("Dimensions").is_none() {
        reported["Dimensions"] = json!([]);
    }
    reported
}

/// The training configuration a READ of a model reports, for a write that named `written` (or
/// named none): present, carrying the time ranges it excludes — the empty list where the write
/// excluded none — and nothing the write left out.
fn reported_configuration(written: Option<&Value>) -> Value {
    let mut reported = written.cloned().unwrap_or_else(|| json!({}));
    if reported.get("ExcludedTimeRanges").is_none() {
        reported["ExcludedTimeRanges"] = json!([]);
    }
    reported
}

#[tokio::test]
async fn every_detector_form_round_trips_through_describe_anomaly_detectors() {
    // Rows differ along which spelling names the detector, which statistic it watches, how
    // many dimensions the metric carries, and what the training configuration and the metric
    // characteristics hold. `Dimensions` is modelled 0..30, so one row carries the full 30.
    let thirty_dimensions: Vec<Value> = (0..30)
        .map(|index| json!({"Name": format!("d{index:02}"), "Value": format!("v{index}")}))
        .collect();
    let cases: Vec<(&str, Value, Option<Value>)> = vec![
        (
            "the single-metric structure with only the members it is identified by",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Stat": "Average",
            }}),
            Some(json!({
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Stat": "Average",
            })),
        ),
        (
            "the single-metric structure with dimensions and a percentile statistic",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [{"Name": "Service", "Value": "checkout"}],
                "Stat": "p99",
            }}),
            Some(json!({
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [{"Name": "Service", "Value": "checkout"}],
                "Stat": "p99",
            })),
        ),
        (
            "the single-metric structure with the 30 dimensions the model allows",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": "Other/Namespace",
                "MetricName": "Fanout",
                "Dimensions": thirty_dimensions.clone(),
                "Stat": "SampleCount",
            }}),
            Some(json!({
                "Namespace": "Other/Namespace",
                "MetricName": "Fanout",
                "Dimensions": thirty_dimensions,
                "Stat": "SampleCount",
            })),
        ),
        (
            "the single-metric structure with an account id and a trimmed-mean statistic",
            json!({"SingleMetricAnomalyDetector": {
                "AccountId": "123456789012",
                "Namespace": NAMESPACE,
                "MetricName": "Throughput",
                "Stat": "TM(25%:75%)",
            }}),
            Some(json!({
                "AccountId": "123456789012",
                "Namespace": NAMESPACE,
                "MetricName": "Throughput",
                "Stat": "TM(25%:75%)",
            })),
        ),
        (
            "the deprecated flat metric members",
            json!({
                "Namespace": NAMESPACE,
                "MetricName": "Errors",
                "Dimensions": [{"Name": "Stage", "Value": "prod"}],
                "Stat": "Sum",
            }),
            Some(json!({
                "Namespace": NAMESPACE,
                "MetricName": "Errors",
                "Dimensions": [{"Name": "Stage", "Value": "prod"}],
                "Stat": "Sum",
            })),
        ),
        (
            "the deprecated flat members with a training configuration and characteristics",
            json!({
                "Namespace": NAMESPACE,
                "MetricName": "Queued",
                "Stat": "Maximum",
                "Configuration": {
                    "ExcludedTimeRanges": [
                        {"StartTime": 1_700_000_000, "EndTime": 1_700_003_600},
                        {"StartTime": 1_700_090_000, "EndTime": 1_700_093_600},
                    ],
                    "MetricTimezone": "Europe/Dublin",
                    "AdaptationSpeed": "SLOW",
                    "OutlierFilteringStrength": "STRICT",
                },
                "MetricCharacteristics": {"PeriodicSpikes": true},
            }),
            Some(json!({
                "Namespace": NAMESPACE,
                "MetricName": "Queued",
                "Stat": "Maximum",
            })),
        ),
        (
            "the metric-math structure",
            json!({"MetricMathAnomalyDetector": metric_math("Latency", "m1 * 2")}),
            None,
        ),
    ];

    for (case, request, expected_single) in cases {
        let router = build_router();
        let id = put_detector(&router, &request, case).await;

        // A metric-math model is listed only when the request asks for that type by name:
        // "If empty, defaults to SINGLE_METRIC."
        // (`DescribeAnomalyDetectorsInput$AnomalyDetectorTypes`)
        let listing = json!({"AnomalyDetectorTypes": ["SINGLE_METRIC", "METRIC_MATH"]});
        let described = ok(&router, "DescribeAnomalyDetectors", &listing).await;
        let detector = detector_with_id(&described, &id).unwrap_or_else(|| {
            panic!("case {case:?}: expected the detector {id} back, got {described}")
        });

        assert_eq!(
            detector.get("StateValue").and_then(Value::as_str),
            Some("PENDING_TRAINING"),
            "case {case:?}: a model whose training has just started is pending training, got \
             {detector}"
        );
        // What the read reports is the identity the write named, IN FULL: the account the
        // series belongs to, and the dimensions it carries — a list of none where the write
        // named none.
        let expected_reported = expected_single.as_ref().map(reported_single);
        assert_eq!(
            detector.get("SingleMetricAnomalyDetector").cloned(),
            expected_reported,
            "case {case:?}: expected the single metric detector {expected_reported:?}, got \
             {detector}"
        );
        // The deprecated flat members report the same metric the structure carries, so a
        // client reading either spelling reads one model.
        for member in ["Namespace", "MetricName", "Stat", "Dimensions"] {
            let flat = detector.get(member).cloned();
            let structured = expected_reported
                .as_ref()
                .and_then(|single| single.get(member).cloned());
            assert_eq!(
                flat, structured,
                "case {case:?}: {member} must mirror the single metric detector, got {detector}"
            );
        }
        // Every model is trained under a configuration, so the member is reported whether or
        // not the write named one, carrying the ranges it excludes.
        let expected_configuration = reported_configuration(request.get("Configuration"));
        assert_eq!(
            detector.get("Configuration").cloned(),
            Some(expected_configuration.clone()),
            "case {case:?}: expected the training configuration {expected_configuration}, got \
             {detector}"
        );
        for member in ["MetricCharacteristics", "MetricMathAnomalyDetector"] {
            assert_eq!(
                detector.get(member).cloned(),
                request.get(member).cloned(),
                "case {case:?}: {member} must round-trip as it was written, got {detector}"
            );
        }

        // Describing by identifier answers the same model, which is what makes the reported
        // identifier the handle on it.
        let by_id = ok(
            &router,
            "DescribeAnomalyDetectors",
            &json!({"AnomalyDetectorIds": [id]}),
        )
        .await;
        assert_eq!(
            reported_ids(&by_id),
            vec![id.clone()],
            "case {case:?}: expected exactly the detector {id} back, got {by_id}"
        );

        // A second write of the same thing UPDATES that model rather than adding a second
        // one, which is why the identifier it reports is the same.
        let again = put_detector(&router, &request, case).await;
        assert_eq!(
            again, id,
            "case {case:?}: a second write of the same detector reports its identifier again"
        );
        let after = ok(&router, "DescribeAnomalyDetectors", &listing).await;
        assert_eq!(
            reported_ids(&after).len(),
            1,
            "case {case:?}: the second write must not add a second model, got {after}"
        );
    }
}

#[tokio::test]
async fn one_metric_and_statistic_is_one_model_however_the_request_spells_it() {
    // Rows differ along the spelling and the dimension ORDER: the same metric, dimensions and
    // statistic is the same model, so each row's write updates the model the first row
    // created rather than adding one.
    let router = build_router();
    let spellings: Vec<(&str, Value)> = vec![
        (
            "the flat members",
            json!({
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [
                    {"Name": "Service", "Value": "checkout"},
                    {"Name": "Stage", "Value": "prod"},
                ],
                "Stat": "Average",
            }),
        ),
        (
            "the structure",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [
                    {"Name": "Service", "Value": "checkout"},
                    {"Name": "Stage", "Value": "prod"},
                ],
                "Stat": "Average",
            }}),
        ),
        (
            "the structure with the dimensions in the other order",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": [
                    {"Name": "Stage", "Value": "prod"},
                    {"Name": "Service", "Value": "checkout"},
                ],
                "Stat": "Average",
            }}),
        ),
        (
            "the structure carrying a training configuration",
            json!({
                "SingleMetricAnomalyDetector": {
                    "Namespace": NAMESPACE,
                    "MetricName": "Latency",
                    "Dimensions": [
                        {"Name": "Service", "Value": "checkout"},
                        {"Name": "Stage", "Value": "prod"},
                    ],
                    "Stat": "Average",
                },
                "Configuration": {"MetricTimezone": "UTC"},
            }),
        ),
    ];

    let mut first: Option<String> = None;
    for (case, request) in spellings {
        let id = put_detector(&router, &request, case).await;
        match &first {
            None => first = Some(id),
            Some(expected) => assert_eq!(
                &id, expected,
                "case {case:?}: the same metric, dimensions and statistic is one model, so its \
                 identifier is {expected}"
            ),
        }
        let described = ok(&router, "DescribeAnomalyDetectors", &json!({})).await;
        assert_eq!(
            reported_ids(&described).len(),
            1,
            "case {case:?}: one metric and statistic is one model, got {described}"
        );
    }

    // A different statistic over the same metric is a DIFFERENT model.
    let other = put_detector(
        &router,
        &json!({"SingleMetricAnomalyDetector": {
            "Namespace": NAMESPACE,
            "MetricName": "Latency",
            "Dimensions": [
                {"Name": "Service", "Value": "checkout"},
                {"Name": "Stage", "Value": "prod"},
            ],
            "Stat": "Maximum",
        }}),
        "another statistic",
    )
    .await;
    assert_ne!(
        Some(&other),
        first.as_ref(),
        "a model over another statistic is another model"
    );
    let described = ok(&router, "DescribeAnomalyDetectors", &json!({})).await;
    assert_eq!(
        reported_ids(&described).len(),
        2,
        "two statistics over one metric are two models, got {described}"
    );
}

#[tokio::test]
async fn training_leaves_pending_training_for_the_state_the_metric_data_implies() {
    // Rows differ along what the metric the model watches has published by the time its
    // training comes due: a model trained on datapoints is trained, and one trained on none
    // has nothing to expect a value by.
    let cases: Vec<(&str, bool, &str)> = vec![
        ("a metric with datapoints", true, "TRAINED"),
        ("a metric with none", false, "TRAINED_INSUFFICIENT_DATA"),
    ];

    for (case, publish, expected) in cases {
        let router = build_router();
        if publish {
            ok(
                &router,
                "PutMetricData",
                &json!({
                    "Namespace": NAMESPACE,
                    "MetricData": [{"MetricName": "Trained", "Value": 7.0}],
                }),
            )
            .await;
        }
        let id = put_detector(
            &router,
            &json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Trained",
                "Stat": "Average",
            }}),
            case,
        )
        .await;

        let described = ok(&router, "DescribeAnomalyDetectors", &json!({})).await;
        let detector = detector_with_id(&described, &id)
            .unwrap_or_else(|| panic!("case {case:?}: expected the detector, got {described}"));
        assert_eq!(
            detector.get("StateValue").and_then(Value::as_str),
            Some("PENDING_TRAINING"),
            "case {case:?}: a caller reading straight after the write still sees the training \
             it started, got {detector}"
        );

        // The transition is the clock's: it is owed a span after the write, whether or not
        // anybody calls.
        wait_for_evaluation();
        let settled = ok(&router, "DescribeAnomalyDetectors", &json!({})).await;
        let detector = detector_with_id(&settled, &id)
            .unwrap_or_else(|| panic!("case {case:?}: expected the detector, got {settled}"));
        assert_eq!(
            detector.get("StateValue").and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected StateValue {expected:?} once training came due, got \
             {detector}"
        );
        // Reading again changes nothing a later caller sees.
        let again = ok(&router, "DescribeAnomalyDetectors", &json!({})).await;
        assert_eq!(
            again, settled,
            "case {case:?}: a second listing reports the same models, got {again}"
        );
    }
}

#[tokio::test]
async fn a_request_naming_no_single_detector_is_refused() {
    // Rows differ along WHICH parameters are combined, and span both writers: the structures
    // together, a structure beside each deprecated flat member, a request naming nothing, and
    // a single-metric form missing one of the three members a metric and statistic needs.
    let single = json!({
        "Namespace": NAMESPACE,
        "MetricName": "Latency",
        "Stat": "Average",
    });
    let math = metric_math("Latency", "m1 * 2");
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "both structures",
            json!({"SingleMetricAnomalyDetector": single, "MetricMathAnomalyDetector": math}),
            Some("InvalidParameterCombinationException"),
        ),
        (
            "the single-metric structure beside a flat Namespace",
            json!({"SingleMetricAnomalyDetector": single, "Namespace": NAMESPACE}),
            Some("InvalidParameterCombinationException"),
        ),
        (
            "the single-metric structure beside a flat MetricName",
            json!({"SingleMetricAnomalyDetector": single, "MetricName": "Latency"}),
            Some("InvalidParameterCombinationException"),
        ),
        (
            "the single-metric structure beside a flat Stat",
            json!({"SingleMetricAnomalyDetector": single, "Stat": "Average"}),
            Some("InvalidParameterCombinationException"),
        ),
        (
            "the single-metric structure beside flat Dimensions",
            json!({
                "SingleMetricAnomalyDetector": single,
                "Dimensions": [{"Name": "Stage", "Value": "prod"}],
            }),
            Some("InvalidParameterCombinationException"),
        ),
        (
            "the metric-math structure beside a flat Namespace",
            json!({"MetricMathAnomalyDetector": math, "Namespace": NAMESPACE}),
            Some("InvalidParameterCombinationException"),
        ),
        (
            "no detector at all",
            json!({}),
            Some("MissingRequiredParameterException"),
        ),
        (
            "a flat form with no MetricName",
            json!({"Namespace": NAMESPACE, "Stat": "Average"}),
            Some("MissingRequiredParameterException"),
        ),
        (
            "a flat form with no Stat",
            json!({"Namespace": NAMESPACE, "MetricName": "Latency"}),
            Some("MissingRequiredParameterException"),
        ),
        (
            "a structure with no Namespace",
            json!({"SingleMetricAnomalyDetector": {
                "MetricName": "Latency",
                "Stat": "Average",
            }}),
            Some("MissingRequiredParameterException"),
        ),
        (
            "a structure with no Stat",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
            }}),
            Some("MissingRequiredParameterException"),
        ),
        ("exactly one structure", json!({"SingleMetricAnomalyDetector": single}), None),
        ("exactly the flat members", single.clone(), None),
        (
            "exactly the metric-math structure",
            json!({"MetricMathAnomalyDetector": math}),
            None,
        ),
    ];

    for (case, request, expected) in cases {
        let router = build_router();
        // Both writers read ONE rule, so each row is asserted against both of them. A row
        // that names one detector is written and then deleted again, which is the same rule
        // reaching the store from both sides.
        for operation in ["PutAnomalyDetector", "DeleteAnomalyDetector"] {
            let (status, body) = call(&router, operation, &request).await;
            match expected {
                Some(expected) => assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, expected),
                    "case {case:?} on {operation}: expected HTTP 400 {expected}, got {status} \
                     with {body}"
                ),
                None => assert_eq!(
                    status, 200,
                    "case {case:?} on {operation}: expected HTTP 200, got {status} with {body}"
                ),
            }
        }
    }
}

#[tokio::test]
async fn the_refusal_names_the_forms_the_request_was_found_carrying() {
    // Rows differ along WHICH forms were mixed, which is the variable part of the refusal: the
    // forms the request carried, listed in ONE fixed order whatever order the request named
    // them in (`crate::state::in_one_order`), so two requests mixing the same forms read the
    // same sentence.
    let single = json!({
        "Namespace": NAMESPACE,
        "MetricName": "Latency",
        "Stat": "Average",
    });
    let math = metric_math("Latency", "m1 * 2");
    let prefix = "Either single metric attributes, SingleMetricAnomalyDetector or \
                  MetricMathAnomalyDetector can be set, found ";
    let cases: Vec<(&str, Value, String)> = vec![
        (
            "the flat members beside the structure",
            json!({"SingleMetricAnomalyDetector": single, "Namespace": NAMESPACE}),
            format!("{prefix}single metric attributes, SingleMetricAnomalyDetector."),
        ),
        (
            "the flat members beside the metric-math structure",
            json!({"MetricMathAnomalyDetector": math, "MetricName": "Latency"}),
            format!("{prefix}MetricMathAnomalyDetector, single metric attributes."),
        ),
        (
            "both structures",
            json!({"SingleMetricAnomalyDetector": single, "MetricMathAnomalyDetector": math}),
            format!("{prefix}MetricMathAnomalyDetector, SingleMetricAnomalyDetector."),
        ),
        (
            "all three forms",
            json!({
                "SingleMetricAnomalyDetector": single,
                "MetricMathAnomalyDetector": math,
                "Stat": "Average",
            }),
            format!(
                "{prefix}MetricMathAnomalyDetector, single metric attributes, \
                 SingleMetricAnomalyDetector."
            ),
        ),
    ];

    for (case, request, expected) in cases {
        let router = build_router();
        // One rule, so the same sentence answers both writers.
        for operation in ["PutAnomalyDetector", "DeleteAnomalyDetector"] {
            let (status, body) = call(&router, operation, &request).await;
            let message = fault_message(&body);
            assert_eq!(
                (status, message.as_str()),
                (400, expected.as_str()),
                "case {case:?} on {operation}: expected HTTP 400 {expected:?}, got {status} \
                 with {body}"
            );
            // The INVARIANT the rows above are instances of: whatever was found, it is listed
            // in the ONE order the service lists these forms in, never in the order the request
            // named them or a handler tested them.
            assert!(
                lists_forms_in_one_fixed_order(&message),
                "case {case:?} on {operation}: expected the forms found to be listed in the \
                 order {THE_ORDER_FORMS_ARE_LISTED_IN:?}, got {message:?}"
            );
        }
    }

    // A model no write created is not found, however the delete named it.
    let router = build_router();
    for named in [
        json!({"AnomalyDetectorId": "0123456789abcdef"}),
        json!({"SingleMetricAnomalyDetector": {
            "Namespace": NAMESPACE,
            "MetricName": "Latency",
            "Stat": "Average",
        }}),
        json!({"Namespace": NAMESPACE, "MetricName": "Latency", "Stat": "Average"}),
        json!({"MetricMathAnomalyDetector": metric_math("Latency", "m1 * 2")}),
    ] {
        let (status, body) = call(&router, "DeleteAnomalyDetector", &named).await;
        assert_eq!(
            (status, fault_message(&body).as_str()),
            (404, "Anomaly detector not found"),
            "{named}: expected HTTP 404 'Anomaly detector not found', got {status} with {body}"
        );
    }
}

/// The ONE order a refusal lists the forms a request was found carrying in, written out here
/// rather than read from the implementation or from the sentence under test.
///
/// SOURCE: the two refusals AWS's wording is measured for — the deprecated flat attributes
/// beside the single-metric structure read "found single metric attributes,
/// SingleMetricAnomalyDetector.", and both structures together read "found
/// MetricMathAnomalyDetector, SingleMetricAnomalyDetector." — so the structure the deprecated
/// attributes are superseded by is listed last, with the identifier only the delete accepts
/// first.
const THE_ORDER_FORMS_ARE_LISTED_IN: [&str; 4] = [
    "AnomalyDetectorId",
    "MetricMathAnomalyDetector",
    "single metric attributes",
    "SingleMetricAnomalyDetector",
];

/// Whether `message` lists the forms the request was FOUND carrying in that order: every form
/// it names is one of them, and no form comes before one the order puts ahead of it.
fn lists_forms_in_one_fixed_order(message: &str) -> bool {
    const BETWEEN: &str = " can be set, found ";
    let Some((_, found)) = message.split_once(BETWEEN) else {
        return false;
    };
    let mut previous: Option<usize> = None;
    for form in found.trim_end_matches('.').split(", ") {
        let Some(at) = THE_ORDER_FORMS_ARE_LISTED_IN
            .iter()
            .position(|listed| *listed == form)
        else {
            return false;
        };
        if previous.is_some_and(|before| at <= before) {
            return false;
        }
        previous = Some(at);
    }
    true
}

#[tokio::test]
async fn a_delete_naming_a_detector_by_identifier_and_by_metric_lists_both() {
    // The delete accepts a FOURTH identification — "If you specify this parameter, you do not
    // need to specify a metric to identify the detector."
    // (`DeleteAnomalyDetectorInput$AnomalyDetectorId`) — so its refusal enumerates four forms,
    // and the rows differ along which of the other three the identifier was mixed with, up to
    // all of them at once. Each sentence lists what was found in the one order the service
    // lists these forms in.
    let single = json!({
        "Namespace": NAMESPACE,
        "MetricName": "Latency",
        "Stat": "Average",
    });
    let math = metric_math("Latency", "m1 * 2");
    let prefix = "Either AnomalyDetectorId, single metric attributes, \
                  SingleMetricAnomalyDetector or MetricMathAnomalyDetector can be set, found \
                  AnomalyDetectorId, ";
    let cases: Vec<(&str, Value, String)> = vec![
        (
            "the identifier beside a flat Stat",
            json!({"AnomalyDetectorId": "abc123", "Stat": "Average"}),
            format!("{prefix}single metric attributes."),
        ),
        (
            "the identifier beside the single-metric structure",
            json!({"AnomalyDetectorId": "abc123", "SingleMetricAnomalyDetector": single}),
            format!("{prefix}SingleMetricAnomalyDetector."),
        ),
        (
            "the identifier beside the metric-math structure",
            json!({"AnomalyDetectorId": "abc123", "MetricMathAnomalyDetector": math}),
            format!("{prefix}MetricMathAnomalyDetector."),
        ),
        (
            "the identifier beside all three",
            json!({
                "AnomalyDetectorId": "abc123",
                "Namespace": NAMESPACE,
                "SingleMetricAnomalyDetector": single,
                "MetricMathAnomalyDetector": math,
            }),
            format!(
                "{prefix}MetricMathAnomalyDetector, single metric attributes, \
                 SingleMetricAnomalyDetector."
            ),
        ),
    ];

    for (case, request, expected) in cases {
        let router = build_router();
        let (status, body) = call(&router, "DeleteAnomalyDetector", &request).await;
        let message = fault_message(&body);
        assert_eq!(
            (status, error_code(&body).as_str(), message.as_str()),
            (400, "InvalidParameterCombinationException", expected.as_str()),
            "case {case:?}: expected HTTP 400 InvalidParameterCombinationException \
             {expected:?}, got {status} with {body}"
        );
        assert!(
            lists_forms_in_one_fixed_order(&message),
            "case {case:?}: expected the forms found to be listed in the order \
             {THE_ORDER_FORMS_ARE_LISTED_IN:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_detector_a_write_cannot_train_is_refused() {
    // Rows differ along what makes the model untrainable: the queries designating no single
    // input series, and a training configuration outside what it may hold. The accepted rows
    // span the bounds themselves — one designated query, and the ten excluded time ranges the
    // configuration allows.
    let ten_ranges: Vec<Value> = (0..10)
        .map(|index| {
            let start = 1_700_000_000 + index * 7_200;
            json!({"StartTime": start, "EndTime": start + 3_600})
        })
        .collect();
    let eleven_ranges: Vec<Value> = (0..11)
        .map(|index| {
            let start = 1_700_000_000 + index * 7_200;
            json!({"StartTime": start, "EndTime": start + 3_600})
        })
        .collect();
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "one designated query",
            json!({"MetricMathAnomalyDetector": metric_math("Latency", "m1 * 2")}),
            None,
        ),
        (
            "no designated query",
            json!({"MetricMathAnomalyDetector": {"MetricDataQueries": [
                {"Id": "ad1", "Expression": "m1 * 2", "ReturnData": false},
            ]}}),
            Some("InvalidParameterValueException"),
        ),
        (
            "two designated queries",
            json!({"MetricMathAnomalyDetector": {"MetricDataQueries": [
                {"Id": "e1", "Expression": "m1 * 2", "ReturnData": true},
                {"Id": "e2", "Expression": "m1 * 3", "ReturnData": true},
            ]}}),
            Some("InvalidParameterValueException"),
        ),
        (
            "no query at all",
            json!({"MetricMathAnomalyDetector": {"MetricDataQueries": []}}),
            Some("InvalidParameterValueException"),
        ),
        (
            "the ten excluded time ranges the configuration allows",
            json!({
                "SingleMetricAnomalyDetector": {
                    "Namespace": NAMESPACE,
                    "MetricName": "Latency",
                    "Stat": "Average",
                },
                "Configuration": {"ExcludedTimeRanges": ten_ranges},
            }),
            None,
        ),
        (
            "eleven excluded time ranges",
            json!({
                "SingleMetricAnomalyDetector": {
                    "Namespace": NAMESPACE,
                    "MetricName": "Latency",
                    "Stat": "Average",
                },
                "Configuration": {"ExcludedTimeRanges": eleven_ranges},
            }),
            Some("InvalidParameterValueException"),
        ),
        (
            "an excluded range that ends where it starts",
            json!({
                "SingleMetricAnomalyDetector": {
                    "Namespace": NAMESPACE,
                    "MetricName": "Latency",
                    "Stat": "Average",
                },
                "Configuration": {"ExcludedTimeRanges": [
                    {"StartTime": 1_700_000_000, "EndTime": 1_700_000_000},
                ]},
            }),
            Some("InvalidParameterValueException"),
        ),
        (
            "an excluded range that ends before it starts",
            json!({
                "SingleMetricAnomalyDetector": {
                    "Namespace": NAMESPACE,
                    "MetricName": "Latency",
                    "Stat": "Average",
                },
                "Configuration": {"ExcludedTimeRanges": [
                    {"StartTime": 1_700_003_600, "EndTime": 1_700_000_000},
                ]},
            }),
            Some("InvalidParameterValueException"),
        ),
    ];

    for (case, request, expected) in cases {
        let router = build_router();
        let (status, body) = call(&router, "PutAnomalyDetector", &request).await;
        match expected {
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {body}"
            ),
            Some(expected) => assert_eq!(
                (status, error_code(&body).as_str()),
                (400, expected),
                "case {case:?}: expected HTTP 400 {expected}, got {status} with {body}"
            ),
        }
    }
}

#[tokio::test]
async fn a_detector_is_deleted_by_identifier_and_by_what_it_watches() {
    // Rows differ along HOW the delete names the model it removes: by the identifier the write
    // reported, by the structure the write used, by the deprecated flat members, by the
    // dimensions in the other order, and by the queries of a metric-math model.
    let dimensions = json!([
        {"Name": "Service", "Value": "checkout"},
        {"Name": "Stage", "Value": "prod"},
    ]);
    let reversed = json!([
        {"Name": "Stage", "Value": "prod"},
        {"Name": "Service", "Value": "checkout"},
    ]);
    let cases: Vec<(&str, Value, Option<Value>)> = vec![
        (
            "by the identifier the write reported",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Stat": "Average",
            }}),
            None,
        ),
        (
            "by the structure the write used",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": dimensions.clone(),
                "Stat": "Average",
            }}),
            Some(json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": dimensions.clone(),
                "Stat": "Average",
            }})),
        ),
        (
            "by the deprecated flat members",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": dimensions.clone(),
                "Stat": "Average",
            }}),
            Some(json!({
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": dimensions.clone(),
                "Stat": "Average",
            })),
        ),
        (
            "by the same dimensions in the other order",
            json!({
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": dimensions,
                "Stat": "Average",
            }),
            Some(json!({
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Dimensions": reversed,
                "Stat": "Average",
            })),
        ),
        (
            "by the queries of a metric-math model",
            json!({"MetricMathAnomalyDetector": metric_math("Latency", "m1 * 2")}),
            Some(json!({"MetricMathAnomalyDetector": metric_math("Latency", "m1 * 2")})),
        ),
    ];

    for (case, write, delete_by) in cases {
        let router = build_router();
        let id = put_detector(&router, &write, case).await;
        let delete_request = delete_by.unwrap_or_else(|| json!({"AnomalyDetectorId": id.clone()}));

        let deleted = ok(&router, "DeleteAnomalyDetector", &delete_request).await;
        assert_eq!(
            deleted,
            json!({}),
            "case {case:?}: DeleteAnomalyDetector models no output member, got {deleted}"
        );
        let described = ok(
            &router,
            "DescribeAnomalyDetectors",
            &json!({"AnomalyDetectorTypes": ["SINGLE_METRIC", "METRIC_MATH"]}),
        )
        .await;
        assert!(
            detector_with_id(&described, &id).is_none(),
            "case {case:?}: the deleted model must be gone, got {described}"
        );

        // Deleting it again finds nothing: "The named resource does not exist."
        let (status, body) = call(&router, "DeleteAnomalyDetector", &delete_request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn an_identifier_cannot_be_combined_with_the_metric_a_delete_names() {
    // Rows differ along WHICH other parameter the identifier is combined with; the identifier
    // on its own is what deletes the model.
    let router = build_router();
    let id = put_detector(
        &router,
        &json!({"SingleMetricAnomalyDetector": {
            "Namespace": NAMESPACE,
            "MetricName": "Latency",
            "Stat": "Average",
        }}),
        "the model the rows delete",
    )
    .await;
    let combinations: Vec<(&str, Value)> = vec![
        ("a Namespace", json!({"Namespace": NAMESPACE})),
        ("a MetricName", json!({"MetricName": "Latency"})),
        ("a Stat", json!({"Stat": "Average"})),
        (
            "Dimensions",
            json!({"Dimensions": [{"Name": "Stage", "Value": "prod"}]}),
        ),
        (
            "a SingleMetricAnomalyDetector",
            json!({"SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": "Latency",
                "Stat": "Average",
            }}),
        ),
        (
            "a MetricMathAnomalyDetector",
            json!({"MetricMathAnomalyDetector": metric_math("Latency", "m1 * 2")}),
        ),
    ];

    for (case, extra) in combinations {
        let mut request = json!({"AnomalyDetectorId": id.clone()});
        for (member, value) in extra.as_object().expect("the row is an object") {
            request[member.as_str()] = value.clone();
        }
        let (status, body) = call(&router, "DeleteAnomalyDetector", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterCombinationException"),
            "case {case:?}: an identifier combined with {case} is refused, got {status} with \
             {body}"
        );
    }

    // The identifier on its own deletes the model the rows left alone.
    ok(
        &router,
        "DeleteAnomalyDetector",
        &json!({"AnomalyDetectorId": id}),
    )
    .await;
}

#[tokio::test]
async fn describe_anomaly_detectors_filters_and_pages_over_what_it_holds() {
    let router = build_router();
    // Three single-metric models and one metric-math model, so every filter below has
    // something it must leave out.
    let latency = put_detector(
        &router,
        &json!({"SingleMetricAnomalyDetector": {
            "Namespace": NAMESPACE,
            "MetricName": "Latency",
            "Dimensions": [{"Name": "Service", "Value": "checkout"}],
            "Stat": "Average",
        }}),
        "latency",
    )
    .await;
    let errors = put_detector(
        &router,
        &json!({"SingleMetricAnomalyDetector": {
            "Namespace": NAMESPACE,
            "MetricName": "Errors",
            "Stat": "Sum",
        }}),
        "errors",
    )
    .await;
    let other = put_detector(
        &router,
        &json!({"SingleMetricAnomalyDetector": {
            "Namespace": "Other/Namespace",
            "MetricName": "Latency",
            "Stat": "Average",
        }}),
        "another namespace",
    )
    .await;
    let math = put_detector(
        &router,
        &json!({"MetricMathAnomalyDetector": metric_math("Latency", "m1 * 2")}),
        "metric math",
    )
    .await;

    let sorted = |ids: &[&String]| {
        let mut ids: Vec<String> = ids.iter().map(|id| (*id).clone()).collect();
        ids.sort();
        ids
    };
    // Rows differ along which filter narrows the listing: the default type, each named type,
    // the namespace, the metric name and the dimensions.
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "no filter at all, which defaults to SINGLE_METRIC",
            json!({}),
            sorted(&[&latency, &errors, &other]),
        ),
        (
            "the metric-math type",
            json!({"AnomalyDetectorTypes": ["METRIC_MATH"]}),
            vec![math.clone()],
        ),
        (
            "both types",
            json!({"AnomalyDetectorTypes": ["SINGLE_METRIC", "METRIC_MATH"]}),
            sorted(&[&latency, &errors, &other, &math]),
        ),
        (
            "one namespace",
            json!({"Namespace": NAMESPACE}),
            sorted(&[&latency, &errors]),
        ),
        (
            "one metric name",
            json!({"MetricName": "Latency"}),
            sorted(&[&latency, &other]),
        ),
        (
            "a namespace and a metric name together",
            json!({"Namespace": NAMESPACE, "MetricName": "Latency"}),
            vec![latency.clone()],
        ),
        (
            "one dimension",
            json!({"Dimensions": [{"Name": "Service", "Value": "checkout"}]}),
            vec![latency.clone()],
        ),
        (
            "a dimension nothing carries",
            json!({"Dimensions": [{"Name": "Service", "Value": "cart"}]}),
            vec![],
        ),
        (
            "a namespace nothing watches",
            json!({"Namespace": "Nothing/Here"}),
            vec![],
        ),
        (
            "two identifiers",
            json!({"AnomalyDetectorIds": [errors.clone(), math.clone()]}),
            sorted(&[&errors, &math]),
        ),
        (
            "an identifier nothing was minted for",
            json!({"AnomalyDetectorIds": ["0123456789abcdef"]}),
            vec![],
        ),
    ];

    for (case, request, expected) in cases {
        let described = ok(&router, "DescribeAnomalyDetectors", &request).await;
        assert_eq!(
            reported_ids(&described),
            expected,
            "case {case:?}: expected exactly {expected:?}, got {described}"
        );
        assert_eq!(
            described.get("NextToken"),
            None,
            "case {case:?}: a listing that fits in one page carries no NextToken, got \
             {described}"
        );
    }

    // Paging: rows differ along the page size, and every page size must walk the same models
    // exactly once in the same order.
    let all = reported_ids(
        &ok(
            &router,
            "DescribeAnomalyDetectors",
            &json!({"AnomalyDetectorTypes": ["SINGLE_METRIC", "METRIC_MATH"]}),
        )
        .await,
    );
    for page_size in [1_usize, 2, 3, 4, 100] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = json!({
                "AnomalyDetectorTypes": ["SINGLE_METRIC", "METRIC_MATH"],
                "MaxResults": page_size,
            });
            if let Some(token) = &token {
                request["NextToken"] = Value::from(token.clone());
            }
            let page = ok(&router, "DescribeAnomalyDetectors", &request).await;
            let ids = reported_ids(&page);
            assert!(
                ids.len() <= page_size,
                "page size {page_size}: a page holds at most that many models, got {page}"
            );
            walked.extend(ids);
            match page.get("NextToken").and_then(Value::as_str) {
                None => break,
                Some(next) => token = Some(next.to_string()),
            }
        }
        assert_eq!(
            walked, all,
            "page size {page_size}: paging must walk every model exactly once, in order"
        );
    }

    // Identifiers name the models themselves, so a metric filter cannot narrow them further.
    for extra in [
        json!({"Namespace": NAMESPACE}),
        json!({"MetricName": "Latency"}),
        json!({"Dimensions": [{"Name": "Service", "Value": "checkout"}]}),
        json!({"AnomalyDetectorTypes": ["METRIC_MATH"]}),
    ] {
        let mut request = json!({"AnomalyDetectorIds": [latency.clone()]});
        for (member, value) in extra.as_object().expect("the row is an object") {
            request[member.as_str()] = value.clone();
        }
        let (status, body) = call(&router, "DescribeAnomalyDetectors", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterCombinationException"),
            "identifiers combined with {extra}: expected HTTP 400 \
             InvalidParameterCombinationException, got {status} with {body}"
        );
    }

    // A token this service did not issue is the InvalidNextToken the operation models.
    for token in ["not-a-token", "zz", "0"] {
        let (status, body) = call(
            &router,
            "DescribeAnomalyDetectors",
            &json!({"NextToken": token}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidNextToken"),
            "token {token:?}: expected HTTP 400 InvalidNextToken, got {status} with {body}"
        );
    }
}
