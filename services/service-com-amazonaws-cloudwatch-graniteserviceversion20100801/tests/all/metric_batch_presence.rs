// SPDX-License-Identifier: CC0-1.0

//! What a publish that carries NO metric datum is answered with.
//!
//! The rule under test: **a `PutMetricData` call that states no metric datum at all publishes
//! nothing, and is refused as the required parameter it was not given — HTTP 400, the
//! missing-parameter fault the operation's model declares
//! (`smithy_query(query="operation",operation="PutMetricData")`: `MissingRequiredParameterException`,
//! `awsQueryError` code `MissingParameter`) and the fixed sentence of that refusal — while the
//! identical call carrying one datum is served.** The datums are counted over BOTH lists one
//! may arrive on, because the metric data an `EntityMetricData` entry carries is a `MetricData`
//! list too ("The EntityMetricData structure includes MetricData structures for the metric
//! data", `PutMetricData`).
//!
//! Why it matters: a client whose batching loop produced no datum would otherwise believe it
//! published, and the no-op is invisible — the refusal is the only signal that a publish round
//! did nothing.
//!
//! The rows differ along what the rule generalises over: HOW the call states no datum (the list
//! submitted empty, the list left out altogether, an entity carrying an empty list, an entity
//! carrying no list at all, and both lists empty at once), and the controls differ along WHICH
//! list carries the one datum that makes the call a publish.

use serde_json::{json, Value};

use crate::harness::{build_router, call, call_in_query_mode, error_code, error_message, ok};

/// The namespace every call in this suite publishes into or reads back.
const NAMESPACE: &str = "MyApp/BatchPresence";

/// The error SHAPE the body of the missing-parameter fault names, and the legacy query code the
/// `x-amzn-query-error` header reports it under for a client that asked for the report.
const MISSING_PARAMETER_SHAPE: &str = "MissingRequiredParameterException";
const MISSING_PARAMETER_QUERY_CODE: &str = "MissingParameter;Sender";

/// The header that report travels in.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The whole sentence such a refusal states: fixed text, with no part of the request in it.
const NO_DATUM_MESSAGE: &str = "At least one MetricDatum object must be present in the request.";

/// One datum the service can hold, under `metric`.
fn datum(metric: &str) -> Value {
    json!({"MetricName": metric, "Value": 1.0, "Unit": "Count"})
}

/// The metrics the catalogue holds in this suite's namespace, which is what says whether a call
/// published anything at all.
async fn published_metrics(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let listed = ok(router, "ListMetrics", &json!({"Namespace": NAMESPACE})).await;
    listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|metric| metric.get("MetricName").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

#[tokio::test]
async fn a_publish_that_states_no_datum_is_refused() {
    // (case, the request body)
    let rows: Vec<(&str, Value)> = vec![
        (
            "the metric data submitted empty",
            json!({"Namespace": NAMESPACE, "MetricData": []}),
        ),
        (
            "no metric data member at all",
            json!({"Namespace": NAMESPACE}),
        ),
        (
            "an entity whose own metric data is empty",
            json!({
                "Namespace": NAMESPACE,
                "StrictEntityValidation": false,
                "EntityMetricData": [{"MetricData": []}],
            }),
        ),
        (
            "an entity carrying no metric data at all",
            json!({
                "Namespace": NAMESPACE,
                "StrictEntityValidation": false,
                "EntityMetricData": [{}],
            }),
        ),
        (
            "both lists submitted empty at once",
            json!({
                "Namespace": NAMESPACE,
                "MetricData": [],
                "StrictEntityValidation": true,
                "EntityMetricData": [],
            }),
        ),
    ];

    for (case, request) in rows {
        // A fresh emulator per row, so what the catalogue holds afterwards is this row's own.
        let router = build_router();
        let (status, body) = call(&router, "PutMetricData", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, MISSING_PARAMETER_SHAPE),
            "case {case:?}: expected HTTP 400 {MISSING_PARAMETER_SHAPE}, got {status} with \
             {body}"
        );
        let stated = error_message(&body);
        assert_eq!(
            stated, NO_DATUM_MESSAGE,
            "case {case:?}: expected {NO_DATUM_MESSAGE:?}, got {stated:?}"
        );
        // A refused publish publishes nothing: the namespace holds no series at all.
        let held = published_metrics(&router).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused publish must create no series, got {held:?}"
        );
    }
}

#[tokio::test]
async fn the_same_call_carrying_one_datum_is_served() {
    // The positive control, over each list a datum may arrive on: the call differs from the
    // refused ones above in the datum alone, and what it published is readable back.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "one datum in the metric data",
            "Served",
            json!({"Namespace": NAMESPACE, "MetricData": [datum("Served")]}),
        ),
        (
            "one datum carried by an entity alone",
            "EntityBorne",
            json!({
                "Namespace": NAMESPACE,
                "StrictEntityValidation": false,
                "EntityMetricData": [{"MetricData": [datum("EntityBorne")]}],
            }),
        ),
        (
            "an empty metric data list beside an entity that carries one",
            "BesideAnEmptyList",
            json!({
                "Namespace": NAMESPACE,
                "MetricData": [],
                "StrictEntityValidation": false,
                "EntityMetricData": [{"MetricData": [datum("BesideAnEmptyList")]}],
            }),
        ),
    ];

    for (case, metric, request) in rows {
        let router = build_router();
        let (status, body) = call(&router, "PutMetricData", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the publish to be served, got {status} with {body}"
        );
        let held = published_metrics(&router).await;
        assert_eq!(
            held,
            vec![metric.to_string()],
            "case {case:?}: expected the published series in the catalogue, got {held:?}"
        );
    }
}

#[tokio::test]
async fn the_refusal_reports_its_legacy_class_to_a_query_client() {
    let router = build_router();
    let (status, headers, body) = call_in_query_mode(
        &router,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": []}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, MISSING_PARAMETER_SHAPE),
        "expected HTTP 400 {MISSING_PARAMETER_SHAPE}, got {status} with {body}"
    );
    let reported = headers
        .get(QUERY_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(
        reported, MISSING_PARAMETER_QUERY_CODE,
        "expected {QUERY_ERROR_HEADER} to read {MISSING_PARAMETER_QUERY_CODE:?}, got \
         {reported:?}"
    );
}
