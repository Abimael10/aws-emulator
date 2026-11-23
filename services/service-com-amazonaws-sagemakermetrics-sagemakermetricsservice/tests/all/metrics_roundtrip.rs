// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SageMaker Metrics core loop: `BatchPutMetrics` writes raw
//! data points, `BatchGetMetrics` reads them back aggregated.
//!
//! Every test drives the real wire path — `MultiServiceRouter::dispatch` with a restJson1
//! request — so routing, deserialization, the modelled constraints and serialization are
//! all exercised, not just the handler bodies.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::ops::types::{
    METRIC_QUERY_RESULT_STATUS_VALUES, PUT_METRICS_ERROR_CODE_VALUES,
};
use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// An instant 20 seconds into a minute, 200 seconds into a five-minute window and 800
/// seconds into an hour, so each `Period` puts it in a different bucket.
const T0: i64 = 1_700_000_000;

/// The bucket keys `T0`, `T0 + 60` and `T0 + 3600` fall in, per `Period`. Hand-computed
/// as `floor(epoch_second / width) * width`.
const MINUTE_BUCKETS: [i64; 3] = [1_699_999_980, 1_700_000_040, 1_700_003_580];
/// `T0` and `T0 + 60` share a five-minute window; `T0 + 3600` starts another.
const FIVE_MINUTE_BUCKETS: [i64; 2] = [1_699_999_800, 1_700_003_400];
/// `T0` and `T0 + 60` share an hour; `T0 + 3600` starts another.
const HOUR_BUCKETS: [i64; 2] = [1_699_999_200, 1_700_002_800];

/// Floats compared with a tolerance: the aggregates are computed, not echoed.
const EPSILON: f64 = 1e-9;

/// One emulator with this service mounted on a single shared engine, exactly as the
/// router mounts it in production, plus that engine — for the one test that inspects the
/// store directly.
fn emulator_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are the framework's business and are not what these tests pin.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One emulator with this service mounted on a single shared engine, exactly as the
/// router mounts it in production.
fn emulator() -> MultiServiceRouter {
    emulator_with_engine().0
}

/// A SageMaker trial-component ARN whose final path segment is `name`.
fn trial_component_arn(name: &str) -> String {
    format!("arn:aws:sagemaker:us-east-1:123456789012:experiment-trial-component/{name}")
}

/// Dispatch one restJson1 request and return `(status, parsed body)`.
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://metrics.sagemaker.us-east-1.amazonaws.com{path}"))
        .header("host", "metrics.sagemaker.us-east-1.amazonaws.com")
        .header("content-type", "application/json")
        // The SigV4 credential scope is how the router picks the service; the signature
        // itself is not verified here (authentication is off above).
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20261021/us-east-1/sagemaker/aws4_request, \
             SignedHeaders=host, Signature=not-verified",
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|e| {
            panic!(
                "{method} {path}: response body is not JSON ({e}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, parsed)
}

/// One `RawMetricData` entry: metric name, epoch-second timestamp, optional step, value.
type RawPoint = (&'static str, i64, Option<i32>, f64);

/// One row of the aggregation table: case name, the query, and what it must answer
/// (`Status`, `XAxisValues`, `MetricValues`).
type AggregationCase = (&'static str, Query, &'static str, Vec<i64>, Vec<f64>);

/// `RawMetricData.Value` as this protocol carries it: a JSON number for a finite value,
/// and the quoted text this protocol's codec reads a float from for an unbounded one (a
/// bare non-finite literal is not JSON at all).
fn wire_value(value: f64) -> Value {
    if value.is_finite() {
        json!(value)
    } else if value > 0.0 {
        json!("Infinity")
    } else {
        json!("-Infinity")
    }
}

/// One `MetricValues` entry as the response carries it: a JSON number, or the quoted text
/// a non-finite double travels as. `NaN` is read too, so a reduction that manufactured one
/// is reported by the assertion that compares it rather than by a parse failure here.
fn metric_value(entry: &Value) -> f64 {
    entry.as_f64().unwrap_or_else(|| match entry.as_str() {
        Some("Infinity") => f64::INFINITY,
        Some("-Infinity") => f64::NEG_INFINITY,
        Some("NaN") => f64::NAN,
        _ => panic!("MetricValues entries are doubles, got {entry:?}"),
    })
}

fn raw_metric_data(points: &[RawPoint]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|(name, timestamp, step, value)| {
                let mut datum = serde_json::Map::new();
                datum.insert("MetricName".to_string(), json!(name));
                datum.insert("Timestamp".to_string(), json!(timestamp));
                datum.insert("Value".to_string(), wire_value(*value));
                if let Some(step) = step {
                    datum.insert("Step".to_string(), json!(step));
                }
                Value::Object(datum)
            })
            .collect(),
    )
}

/// `PUT /BatchPutMetrics`, returning the per-index errors it reported as
/// `(MetricIndex, Code)` pairs — empty when the whole batch was accepted.
async fn put_metrics(
    router: &MultiServiceRouter,
    trial_component: &str,
    points: &[RawPoint],
) -> Vec<(i64, String)> {
    let body = json!({
        "TrialComponentName": trial_component,
        "MetricData": raw_metric_data(points),
    });
    let (status, response) = call(router, "PUT", "/BatchPutMetrics", &body).await;
    assert_eq!(
        status, 200,
        "BatchPutMetrics({trial_component}) must answer 200, got {status}: {response:?}"
    );
    match response.get("Errors") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(errors)) => errors
            .iter()
            .map(|error| {
                let index = error["MetricIndex"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("error entry without MetricIndex: {error:?}"));
                let code = error["Code"]
                    .as_str()
                    .unwrap_or_else(|| panic!("error entry without Code: {error:?}"))
                    .to_string();
                assert!(
                    PUT_METRICS_ERROR_CODE_VALUES.contains(&code.as_str()),
                    "error code {code:?} is not a modelled PutMetricsErrorCode"
                );
                (index, code)
            })
            .collect(),
        other => panic!("Errors must be a list, got {other:?}"),
    }
}

/// One `MetricQuery`, as a test writes it.
#[derive(Debug, Clone)]
struct Query {
    metric: &'static str,
    resource_arn: String,
    stat: &'static str,
    period: &'static str,
    axis: &'static str,
    start: Option<i64>,
    end: Option<i64>,
}

impl Query {
    fn new(metric: &'static str, trial_component: &str) -> Self {
        Self {
            metric,
            resource_arn: trial_component_arn(trial_component),
            stat: "Avg",
            period: "IterationNumber",
            axis: "IterationNumber",
            start: None,
            end: None,
        }
    }

    fn aggregate(mut self, stat: &'static str, period: &'static str, axis: &'static str) -> Self {
        self.stat = stat;
        self.period = period;
        self.axis = axis;
        self
    }

    fn window(mut self, start: Option<i64>, end: Option<i64>) -> Self {
        self.start = start;
        self.end = end;
        self
    }

    fn arn(mut self, resource_arn: &str) -> Self {
        self.resource_arn = resource_arn.to_string();
        self
    }

    fn to_json(&self) -> Value {
        let mut query = serde_json::Map::new();
        query.insert("MetricName".to_string(), json!(self.metric));
        query.insert("ResourceArn".to_string(), json!(self.resource_arn));
        query.insert("MetricStat".to_string(), json!(self.stat));
        query.insert("Period".to_string(), json!(self.period));
        query.insert("XAxisType".to_string(), json!(self.axis));
        if let Some(start) = self.start {
            query.insert("Start".to_string(), json!(start));
        }
        if let Some(end) = self.end {
            query.insert("End".to_string(), json!(end));
        }
        Value::Object(query)
    }
}

/// One `MetricQueryResult`, as a test reads it. The two value lists are `Option` because
/// a result reporting a query's FAILURE carries neither member.
#[derive(Debug, Clone, PartialEq)]
struct QueryResult {
    status: String,
    message: Option<String>,
    x_axis_values: Option<Vec<i64>>,
    metric_values: Option<Vec<f64>>,
}

/// `POST /BatchGetMetrics`, returning one result per query, positionally aligned.
async fn get_metrics(router: &MultiServiceRouter, queries: &[Query]) -> Vec<QueryResult> {
    let body = json!({
        "MetricQueries": queries.iter().map(Query::to_json).collect::<Vec<_>>(),
    });
    let (status, response) = call(router, "POST", "/BatchGetMetrics", &body).await;
    assert_eq!(
        status, 200,
        "BatchGetMetrics must answer 200, got {status}: {response:?}"
    );
    let results = response["MetricQueryResults"]
        .as_array()
        .unwrap_or_else(|| panic!("BatchGetMetrics must return MetricQueryResults: {response:?}"))
        .clone();
    assert_eq!(
        results.len(),
        queries.len(),
        "one result per query, positionally aligned: {results:?}"
    );
    results
        .iter()
        .map(|result| {
            let status = result["Status"]
                .as_str()
                .unwrap_or_else(|| panic!("result without Status: {result:?}"))
                .to_string();
            assert!(
                METRIC_QUERY_RESULT_STATUS_VALUES.contains(&status.as_str()),
                "status {status:?} is not a modelled MetricQueryResultStatus"
            );
            let x_axis_values = result.get("XAxisValues").map(|values| {
                values
                    .as_array()
                    .unwrap_or_else(|| panic!("XAxisValues must be a list: {result:?}"))
                    .iter()
                    .map(|x| x.as_i64().expect("XAxisValues entries are longs"))
                    .collect()
            });
            let metric_values = result.get("MetricValues").map(|values| {
                values
                    .as_array()
                    .unwrap_or_else(|| panic!("MetricValues must be a list: {result:?}"))
                    .iter()
                    .map(metric_value)
                    .collect()
            });
            QueryResult {
                status,
                message: result
                    .get("Message")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                x_axis_values,
                metric_values,
            }
        })
        .collect()
}

/// Assert one SUCCESSFUL result against the case it answers: both value lists are present,
/// the same length, and hold what the case expects.
fn assert_result(case: &str, got: &QueryResult, status: &str, x: &[i64], values: &[f64]) {
    assert_eq!(
        got.status, status,
        "case {case}: expected Status {status}, got {got:?}"
    );
    let got_x = got
        .x_axis_values
        .as_deref()
        .unwrap_or_else(|| panic!("case {case}: an answered query carries XAxisValues: {got:?}"));
    let got_values = got
        .metric_values
        .as_deref()
        .unwrap_or_else(|| panic!("case {case}: an answered query carries MetricValues: {got:?}"));
    assert_eq!(
        got_x, x,
        "case {case}: expected XAxisValues {x:?}, got {got_x:?}"
    );
    assert_eq!(
        got_values.len(),
        values.len(),
        "case {case}: expected {} MetricValues, got {got_values:?}",
        values.len(),
    );
    assert_eq!(
        got_x.len(),
        got_values.len(),
        "case {case}: XAxisValues and MetricValues must be the same length, got {got:?}"
    );
    for (i, (actual, want)) in got_values.iter().zip(values).enumerate() {
        // A computed finite aggregate is compared with a tolerance; an unbounded one is
        // the exact value the statistic names, and NOT A NUMBER equals nothing at all, so
        // a reduction that produced one fails here with the value it produced.
        let answers_the_case = if want.is_finite() {
            (actual - want).abs() < EPSILON
        } else {
            actual == want
        };
        assert!(
            answers_the_case,
            "case {case}: MetricValues[{i}] expected {want}, got {actual}"
        );
    }
}

/// Assert one FAILING result: the status, a `Message` saying what, and NO value lists at
/// all — the channel a client tells a failed query from an empty one by.
fn assert_failed_result(case: &str, got: &QueryResult, status: &str) -> String {
    assert_eq!(
        got.status, status,
        "case {case}: expected Status {status}, got {got:?}"
    );
    assert_eq!(
        got.x_axis_values, None,
        "case {case}: a failing result carries no XAxisValues, got {got:?}"
    );
    assert_eq!(
        got.metric_values, None,
        "case {case}: a failing result carries no MetricValues, got {got:?}"
    );
    got.message
        .clone()
        .unwrap_or_else(|| panic!("case {case}: a failing result carries a Message: {got:?}"))
}

/// The whole aggregation surface: every `MetricStat` x `Period` x `XAxisType` the model
/// lists, over series that differ in bucket spread, step presence and epoch sign.
#[tokio::test]
async fn get_metrics_aggregates_the_points_put_for_the_resource() {
    let router = emulator();
    let component = "tc-aggregate";

    // Three points one second apart: one bucket on every timestamp period, three on
    // IterationNumber.
    let dense: &[RawPoint] = &[
        ("loss", T0, Some(0), 1.0),
        ("loss", T0 + 1, Some(1), 0.5),
        ("loss", T0 + 2, Some(2), 0.25),
    ];
    // Three points spread a minute and an hour apart: the periods disagree about how
    // many buckets there are.
    let spread: &[RawPoint] = &[
        ("wide", T0, Some(0), 2.0),
        ("wide", T0 + 60, Some(1), 4.0),
        ("wide", T0 + 3600, Some(2), 6.0),
    ];
    // A point written without a Step has no iteration number.
    let stepless: &[RawPoint] = &[("partial", T0, None, 5.0), ("partial", T0 + 1, Some(0), 7.0)];
    // Pre-1970 instants: bucket keys must floor towards negative infinity.
    let pre_epoch: &[RawPoint] = &[("old", -90, Some(0), 1.5), ("old", -30, Some(1), 3.5)];

    for batch in [dense, spread, stepless, pre_epoch] {
        let errors = put_metrics(&router, component, batch).await;
        assert!(
            errors.is_empty(),
            "a well-formed batch is accepted whole, got errors {errors:?}"
        );
    }

    // case, query, expected status, expected XAxisValues, expected MetricValues.
    let hour = "OneHour";
    let ts = "Timestamp";
    let iter = "IterationNumber";
    let cases: Vec<AggregationCase> = vec![
        (
            "last-by-iteration",
            Query::new("loss", component).aggregate("Last", iter, iter),
            "Complete",
            vec![0, 1, 2],
            vec![1.0, 0.5, 0.25],
        ),
        (
            "min-one-hour",
            Query::new("loss", component).aggregate("Min", hour, ts),
            "Complete",
            vec![HOUR_BUCKETS[0]],
            vec![0.25],
        ),
        (
            "max-one-hour",
            Query::new("loss", component).aggregate("Max", hour, ts),
            "Complete",
            vec![HOUR_BUCKETS[0]],
            vec![1.0],
        ),
        (
            "avg-one-hour",
            Query::new("loss", component).aggregate("Avg", hour, ts),
            "Complete",
            vec![HOUR_BUCKETS[0]],
            vec![(1.0 + 0.5 + 0.25) / 3.0],
        ),
        (
            "count-one-hour",
            Query::new("loss", component).aggregate("Count", hour, ts),
            "Complete",
            vec![HOUR_BUCKETS[0]],
            vec![3.0],
        ),
        (
            "stddev-one-hour",
            Query::new("loss", component).aggregate("StdDev", hour, ts),
            "Complete",
            vec![HOUR_BUCKETS[0]],
            // Population standard deviation of {1.0, 0.5, 0.25} about its mean.
            vec![{
                let mean = (1.0 + 0.5 + 0.25) / 3.0;
                let variance = [1.0_f64, 0.5, 0.25]
                    .iter()
                    .map(|v| (v - mean) * (v - mean))
                    .sum::<f64>()
                    / 3.0;
                variance.sqrt()
            }],
        ),
        (
            "last-one-hour",
            Query::new("loss", component).aggregate("Last", hour, ts),
            "Complete",
            vec![HOUR_BUCKETS[0]],
            vec![0.25],
        ),
        (
            "spread-one-minute",
            Query::new("wide", component).aggregate("Avg", "OneMinute", ts),
            "Complete",
            MINUTE_BUCKETS.to_vec(),
            vec![2.0, 4.0, 6.0],
        ),
        (
            "spread-five-minute-avg",
            Query::new("wide", component).aggregate("Avg", "FiveMinute", ts),
            "Complete",
            FIVE_MINUTE_BUCKETS.to_vec(),
            vec![3.0, 6.0],
        ),
        (
            "spread-five-minute-count",
            Query::new("wide", component).aggregate("Count", "FiveMinute", ts),
            "Complete",
            FIVE_MINUTE_BUCKETS.to_vec(),
            vec![2.0, 1.0],
        ),
        (
            "spread-five-minute-stddev",
            Query::new("wide", component).aggregate("StdDev", "FiveMinute", ts),
            "Complete",
            FIVE_MINUTE_BUCKETS.to_vec(),
            vec![1.0, 0.0],
        ),
        (
            "spread-one-hour-min",
            Query::new("wide", component).aggregate("Min", hour, ts),
            "Complete",
            HOUR_BUCKETS.to_vec(),
            vec![2.0, 6.0],
        ),
        (
            "spread-by-iteration",
            Query::new("wide", component).aggregate("Max", iter, iter),
            "Complete",
            vec![0, 1, 2],
            vec![2.0, 4.0, 6.0],
        ),
        (
            "spread-window-from-second",
            Query::new("wide", component)
                .aggregate("Avg", "OneMinute", ts)
                .window(Some(T0 + 1), None),
            "Complete",
            vec![MINUTE_BUCKETS[1], MINUTE_BUCKETS[2]],
            vec![4.0, 6.0],
        ),
        (
            "spread-window-single-iteration",
            Query::new("wide", component)
                .aggregate("Avg", iter, iter)
                .window(Some(1), Some(1)),
            "Complete",
            vec![1],
            vec![4.0],
        ),
        (
            "dense-window-up-to-iteration-one",
            Query::new("loss", component)
                .aggregate("Avg", iter, iter)
                .window(None, Some(1)),
            "Complete",
            vec![0, 1],
            vec![1.0, 0.5],
        ),
        (
            // A point with no Step has no position on the iteration axis.
            "stepless-by-iteration",
            Query::new("partial", component).aggregate("Avg", iter, iter),
            "Complete",
            vec![0],
            vec![7.0],
        ),
        (
            // ...but it is still a point in time.
            "stepless-by-timestamp",
            Query::new("partial", component).aggregate("Avg", hour, ts),
            "Complete",
            vec![HOUR_BUCKETS[0]],
            vec![6.0],
        ),
        (
            "pre-epoch-minute-buckets",
            Query::new("old", component).aggregate("Avg", "OneMinute", ts),
            "Complete",
            vec![-120, -60],
            vec![1.5, 3.5],
        ),
        (
            // Period and XAxisType must agree about the iteration axis, so the rows that
            // paired them differently now belong to the pairing table in
            // `envelope_and_per_item_reporting.rs`. What is still true here is that a
            // timestamp period reads and windows on the timestamp axis.
            "spread-window-to-second",
            Query::new("wide", component)
                .aggregate("Avg", "OneMinute", ts)
                .window(None, Some(T0 + 60)),
            "Complete",
            vec![MINUTE_BUCKETS[0], MINUTE_BUCKETS[1]],
            vec![2.0, 4.0],
        ),
        (
            // Nothing was ever written for this metric: an empty answer, not a failure.
            "unknown-metric",
            Query::new("accuracy", component).aggregate("Avg", hour, ts),
            "Complete",
            vec![],
            vec![],
        ),
        (
            // Nor for this resource.
            "unknown-resource",
            Query::new("loss", "tc-never-written").aggregate("Avg", hour, ts),
            "Complete",
            vec![],
            vec![],
        ),
    ];

    for (case, query, status, x, values) in cases {
        let results = get_metrics(&router, std::slice::from_ref(&query)).await;
        assert_result(case, &results[0], status, &x, &values);
    }
}

/// A query the service reports a failure for fails for ITSELF, and its siblings in the same
/// batch are still answered from state — and a query that merely selects nothing is
/// answered as the empty result it is.
#[tokio::test]
async fn a_failing_query_does_not_fail_its_siblings() {
    let router = emulator();
    let component = "tc-isolation";
    let errors = put_metrics(
        &router,
        component,
        &[("loss", T0, Some(0), 1.0), ("loss", T0 + 1, Some(1), 0.5)],
    )
    .await;
    assert!(errors.is_empty(), "batch must be accepted, got {errors:?}");

    let good = Query::new("loss", component).aggregate("Last", "IterationNumber", "IterationNumber");
    let queries = vec![
        // The one failure a query reports: its Period and XAxisType disagree about the
        // iteration axis.
        good.clone().aggregate("Last", "IterationNumber", "Timestamp"),
        good.clone(),
        // Selects nothing, which is not a failure: an ARN the modelled @pattern accepts
        // but that names no resource, and a window that cannot contain anything.
        good.clone()
            .arn("arn:aws:sagemaker:us-east-1:123456789012:experiment-trial-component/"),
        good.clone().window(Some(5), Some(1)),
        good.clone(),
    ];
    let results = get_metrics(&router, &queries).await;

    let message = assert_failed_result("contradicting-axis-members", &results[0], "ValidationError");
    assert!(
        message.contains("IterationNumber"),
        "the Message names the member that must be IterationNumber, got {message:?}"
    );
    for (index, case) in [(2, "arn-without-name"), (3, "start-after-end")] {
        assert_result(case, &results[index], "Complete", &[], &[]);
        assert_eq!(
            results[index].message, None,
            "case {case}: a query that selects nothing says nothing, got {:?}",
            results[index]
        );
    }
    for index in [1, 4] {
        assert_result(
            "sibling-of-a-failed-query",
            &results[index],
            "Complete",
            &[0, 1],
            &[1.0, 0.5],
        );
    }
}

/// `BatchPutMetrics` is partially successful: a point it cannot store is reported by
/// index and every other point in the same call is persisted.
#[tokio::test]
async fn put_metrics_reports_per_index_failures_and_persists_the_rest() {
    let router = emulator();
    let component = "tc-partial";

    // The baseline the conflict and the idempotent re-write are measured against.
    let errors = put_metrics(&router, component, &[("loss", T0, Some(0), 1.0)]).await;
    assert!(errors.is_empty(), "baseline must be accepted, got {errors:?}");

    let batch: &[RawPoint] = &[
        // A point that names no series: nothing to store it in.
        ("   ", T0 + 5, Some(5), 9.0),
        // A new coordinate: stored.
        ("loss", T0 + 1, Some(1), 0.5),
        // The baseline's coordinate with a different value: a conflict, and the stored
        // value must not change.
        ("loss", T0, Some(0), 42.0),
        // The baseline's coordinate with the SAME value: idempotent, not an error.
        ("loss", T0, Some(0), 1.0),
    ];
    let errors = put_metrics(&router, component, batch).await;
    assert_eq!(
        errors,
        vec![
            (0, "INTERNAL_ERROR".to_string()),
            (2, "CONFLICT_ERROR".to_string()),
        ],
        "expected a per-index INTERNAL_ERROR and CONFLICT_ERROR, got {errors:?}"
    );

    let results = get_metrics(
        &router,
        &[Query::new("loss", component).aggregate("Last", "IterationNumber", "IterationNumber")],
    )
    .await;
    assert_result(
        "partial-batch-persisted",
        &results[0],
        "Complete",
        &[0, 1],
        &[1.0, 0.5],
    );
}

/// State survives across requests: a second call adds to the series the first wrote,
/// and the series stays ordered however the points arrive.
#[tokio::test]
async fn points_accumulate_across_requests_in_timestamp_order() {
    let router = emulator();
    let component = "tc-persistence";

    // Deliberately out of order within and across calls.
    for batch in [
        &[("loss", T0 + 2, Some(2), 0.25)] as &[RawPoint],
        &[("loss", T0, Some(0), 1.0)],
        &[("loss", T0 + 3, Some(3), 0.125), ("loss", T0 + 1, Some(1), 0.5)],
    ] {
        let errors = put_metrics(&router, component, batch).await;
        assert!(errors.is_empty(), "batch must be accepted, got {errors:?}");
    }

    let results = get_metrics(
        &router,
        &[Query::new("loss", component).aggregate("Last", "IterationNumber", "IterationNumber")],
    )
    .await;
    assert_result(
        "accumulated-across-requests",
        &results[0],
        "Complete",
        &[0, 1, 2, 3],
        &[1.0, 0.5, 0.25, 0.125],
    );

    // `Last` over a single bucket is the latest point by (timestamp, step), whatever
    // order the puts arrived in.
    let results = get_metrics(
        &router,
        &[Query::new("loss", component).aggregate("Last", "OneHour", "Timestamp")],
    )
    .await;
    assert_result(
        "last-is-the-latest-point",
        &results[0],
        "Complete",
        &[HOUR_BUCKETS[0]],
        &[0.125],
    );
}

/// More matching buckets than one result may carry is reported `Truncated`, with the
/// lowest `MAX_RESULT_POINTS_PER_QUERY` of them in x.
#[tokio::test]
async fn a_result_past_the_point_cap_is_truncated() {
    use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::ops::batch_get_metrics::MAX_RESULT_POINTS_PER_QUERY;

    let router = emulator();
    let component = "tc-truncation";
    let over_cap = MAX_RESULT_POINTS_PER_QUERY + 10;

    // `MetricData` is modelled 1..10, so the series is built ten points at a time.
    let mut step = 0_i32;
    while usize::try_from(step).expect("a non-negative step") < over_cap {
        let batch: Vec<RawPoint> = (0..10)
            .map(|offset| {
                let step = step + offset;
                ("loss", T0 + i64::from(step), Some(step), f64::from(step))
            })
            .collect();
        let errors = put_metrics(&router, component, &batch).await;
        assert!(errors.is_empty(), "batch must be accepted, got {errors:?}");
        step += 10;
    }

    let results = get_metrics(
        &router,
        &[Query::new("loss", component).aggregate("Avg", "IterationNumber", "IterationNumber")],
    )
    .await;
    let result = &results[0];
    assert_eq!(
        result.status, "Truncated",
        "a result past the cap is Truncated, got {result:?}"
    );
    let x_axis_values = result
        .x_axis_values
        .as_deref()
        .unwrap_or_else(|| panic!("a truncated result carries XAxisValues, got {result:?}"));
    assert_eq!(
        x_axis_values.len(),
        MAX_RESULT_POINTS_PER_QUERY,
        "a truncated result carries exactly the cap"
    );
    assert_eq!(
        x_axis_values.first().copied(),
        Some(0),
        "truncation keeps the lowest x values, got {:?}",
        x_axis_values.first()
    );
    assert_eq!(
        x_axis_values.last().copied(),
        Some(i64::try_from(MAX_RESULT_POINTS_PER_QUERY).expect("the cap fits in a long") - 1),
        "truncation keeps the lowest x values, got {:?}",
        x_axis_values.last()
    );
    assert!(
        result.message.is_some(),
        "a truncated result says so in its Message, got {result:?}"
    );
}

/// A metric name past the per-trial-component cap is refused for that index with
/// `METRIC_LIMIT_EXCEEDED`, and the names already stored keep working.
#[tokio::test]
async fn a_metric_name_past_the_cap_is_refused_for_that_index() {
    use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::state::MAX_METRIC_NAMES_PER_TRIAL_COMPONENT;

    let router = emulator();
    let component = "tc-limit";

    // Fill the component to its cap, ten metric names per call.
    const NAMES: [&str; 50] = [
        "m00", "m01", "m02", "m03", "m04", "m05", "m06", "m07", "m08", "m09", "m10", "m11", "m12",
        "m13", "m14", "m15", "m16", "m17", "m18", "m19", "m20", "m21", "m22", "m23", "m24", "m25",
        "m26", "m27", "m28", "m29", "m30", "m31", "m32", "m33", "m34", "m35", "m36", "m37", "m38",
        "m39", "m40", "m41", "m42", "m43", "m44", "m45", "m46", "m47", "m48", "m49",
    ];
    assert!(
        MAX_METRIC_NAMES_PER_TRIAL_COMPONENT <= NAMES.len(),
        "this test needs more distinct names than the cap"
    );
    for chunk in NAMES[..MAX_METRIC_NAMES_PER_TRIAL_COMPONENT].chunks(10) {
        let batch: Vec<RawPoint> = chunk
            .iter()
            .map(|name| (*name, T0, Some(0), 1.0))
            .collect();
        let errors = put_metrics(&router, component, &batch).await;
        assert!(
            errors.is_empty(),
            "a batch within the cap is accepted, got {errors:?}"
        );
    }

    // One more name does not fit; a point on a name already stored still does.
    let past_cap = NAMES[MAX_METRIC_NAMES_PER_TRIAL_COMPONENT];
    let errors = put_metrics(
        &router,
        component,
        &[(past_cap, T0, Some(0), 1.0), (NAMES[0], T0 + 1, Some(1), 2.0)],
    )
    .await;
    assert_eq!(
        errors,
        vec![(0, "METRIC_LIMIT_EXCEEDED".to_string())],
        "only the name past the cap fails, got {errors:?}"
    );

    let results = get_metrics(
        &router,
        &[
            Query::new(NAMES[0], component).aggregate("Last", "IterationNumber", "IterationNumber"),
            Query::new(past_cap, component).aggregate("Last", "IterationNumber", "IterationNumber"),
        ],
    )
    .await;
    assert_result(
        "name-within-the-cap",
        &results[0],
        "Complete",
        &[0, 1],
        &[1.0, 2.0],
    );
    assert_result("name-past-the-cap", &results[1], "Complete", &[], &[]);
}

/// A trial component past the store-wide cap is refused for that index, and the refused
/// datum leaves NOTHING behind: the store must not grow without bound on caller-chosen
/// names.
#[tokio::test]
async fn a_trial_component_past_the_cap_is_refused_and_leaves_no_entry() {
    use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::state::MAX_TRIAL_COMPONENTS;
    use service_com_amazonaws_sagemakermetrics_sagemakermetricsservice::ComAmazonawsSagemakermetricsSagemakermetricsserviceState as MetricsState;

    let (router, engine) = emulator_with_engine();
    for nth in 0..MAX_TRIAL_COMPONENTS {
        let component = format!("tc-cap-{nth}");
        let errors = put_metrics(&router, &component, &[("loss", T0, Some(0), 1.0)]).await;
        assert!(
            errors.is_empty(),
            "component {nth} is within the cap and must be accepted, got {errors:?}"
        );
    }

    let errors = put_metrics(&router, "tc-cap-over", &[("loss", T0, Some(0), 1.0)]).await;
    assert_eq!(
        errors,
        vec![(0, "METRIC_LIMIT_EXCEEDED".to_string())],
        "a component past the cap is refused for its index, got {errors:?}"
    );
    let stored = engine.state::<MetricsState>().lock().components.len();
    assert_eq!(
        stored, MAX_TRIAL_COMPONENTS,
        "a refused datum must not create a trial component entry, got {stored} entries"
    );

    // A component already in the store still accepts points.
    let errors = put_metrics(&router, "tc-cap-0", &[("loss", T0 + 1, Some(1), 2.0)]).await;
    assert!(
        errors.is_empty(),
        "a component within the cap keeps working, got {errors:?}"
    );
    let results = get_metrics(
        &router,
        &[Query::new("loss", "tc-cap-0").aggregate("Last", "IterationNumber", "IterationNumber")],
    )
    .await;
    assert_result(
        "component-within-the-cap",
        &results[0],
        "Complete",
        &[0, 1],
        &[1.0, 2.0],
    );
}

/// What a request that breaks one modelled constraint must be answered with, now that the
/// framework's centralized walk judges none of this service's input members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServedAs {
    /// `BatchGetMetrics`: the one query is answered with this `Status`.
    Query(&'static str),
    /// `BatchPutMetrics`: every point was ingested, so `Errors` is the empty list.
    Ingested,
}

/// A value outside a modelled constraint is NOT refused at the request boundary: this
/// service judges its own members, per item, inside a 200 response (`skip_validation` in
/// `src/ops/types`). This pins where that boundary is, which is why the two handlers do the
/// judging — and the one exception, a reference that is not a SageMaker ARN, is pinned in
/// `resource_arn_refusal.rs`.
#[tokio::test]
async fn a_modelled_constraint_violation_is_reported_per_item_inside_a_200() {
    let router = emulator();
    let arn = trial_component_arn("tc-boundary");
    // A name far past `MetricName`'s modelled maximum of 255.
    let long_name = "n".repeat(400);

    // case, method, path, body, what it must be answered with — one row per constraint
    // class the model declares on a member of either input.
    let cases: Vec<(&str, &str, &str, Value, ServedAs)> = vec![
        (
            // MetricStatistic has no `Median` member: a value this service cannot read as
            // the enumerated type names no statistic, so the query selects nothing.
            "metric-stat-outside-the-enum",
            "POST",
            "/BatchGetMetrics",
            json!({ "MetricQueries": [{
                "MetricName": "loss",
                "ResourceArn": arn,
                "MetricStat": "Median",
                "Period": "IterationNumber",
                "XAxisType": "IterationNumber",
            }] }),
            ServedAs::Query("Complete"),
        ),
        (
            // Period's value set likewise.
            "period-outside-the-enum",
            "POST",
            "/BatchGetMetrics",
            json!({ "MetricQueries": [{
                "MetricName": "loss",
                "ResourceArn": arn,
                "MetricStat": "Last",
                "Period": "OneDay",
                "XAxisType": "Timestamp",
            }] }),
            ServedAs::Query("Complete"),
        ),
        (
            // MetricName is modelled @length 1..255 @pattern '^.+$': a name with no
            // character names no series, which matches nothing.
            "metric-name-empty",
            "POST",
            "/BatchGetMetrics",
            json!({ "MetricQueries": [{
                "MetricName": "",
                "ResourceArn": arn,
                "MetricStat": "Last",
                "Period": "IterationNumber",
                "XAxisType": "IterationNumber",
            }] }),
            ServedAs::Query("Complete"),
        ),
        (
            "metric-name-past-its-length",
            "POST",
            "/BatchGetMetrics",
            json!({ "MetricQueries": [{
                "MetricName": long_name,
                "ResourceArn": arn,
                "MetricStat": "Last",
                "Period": "IterationNumber",
                "XAxisType": "IterationNumber",
            }] }),
            ServedAs::Query("Complete"),
        ),
        (
            // ExperimentEntityName must be entirely lowercase; the point is still ingested.
            "uppercase-trial-component-name",
            "PUT",
            "/BatchPutMetrics",
            json!({
                "TrialComponentName": "TC-Boundary",
                "MetricData": raw_metric_data(&[("loss", T0, Some(0), 1.0)]),
            }),
            ServedAs::Ingested,
        ),
        (
            // Step is modelled @range min 0.
            "negative-step",
            "PUT",
            "/BatchPutMetrics",
            json!({
                "TrialComponentName": "tc-boundary",
                "MetricData": raw_metric_data(&[("loss", T0, Some(-1), 1.0)]),
            }),
            ServedAs::Ingested,
        ),
        (
            "metric-name-past-its-length-on-a-point",
            "PUT",
            "/BatchPutMetrics",
            json!({
                "TrialComponentName": "tc-boundary",
                "MetricData": [{
                    "MetricName": long_name,
                    "Timestamp": T0,
                    "Step": 0,
                    "Value": 1.0,
                }],
            }),
            ServedAs::Ingested,
        ),
        (
            // RawMetricDataList is modelled @length 1..10; an eleventh point is ingested
            // like the rest.
            "more-points-than-the-modelled-maximum",
            "PUT",
            "/BatchPutMetrics",
            json!({
                "TrialComponentName": "tc-boundary",
                "MetricData": Value::Array(
                    (0..11)
                        .map(|step| json!({
                            "MetricName": "many",
                            "Timestamp": T0 + i64::from(step),
                            "Step": step,
                            "Value": f64::from(step),
                        }))
                        .collect(),
                ),
            }),
            ServedAs::Ingested,
        ),
    ];

    for (case, method, path, body, served) in cases {
        let (status, response) = call(&router, method, path, &body).await;
        assert_eq!(
            status, 200,
            "case {case}: a modelled-constraint violation is served and reported per item, \
             got {status}: {response:?}"
        );
        match served {
            ServedAs::Query(expected) => {
                let results = response["MetricQueryResults"].as_array().unwrap_or_else(|| {
                    panic!("case {case}: expected MetricQueryResults, got {response:?}")
                });
                assert_eq!(
                    results.len(),
                    1,
                    "case {case}: one result per query, got {results:?}"
                );
                assert_eq!(
                    results[0]["Status"].as_str(),
                    Some(expected),
                    "case {case}: expected Status {expected}, got {:?}",
                    results[0]
                );
            }
            ServedAs::Ingested => {
                assert_eq!(
                    response["Errors"].as_array().map(Vec::len),
                    Some(0),
                    "case {case}: every point was ingested, so Errors is [], got {response:?}"
                );
            }
        }
    }
}

/// One row of the aggregation table for a bucket holding values of UNBOUNDED magnitude:
/// the case — which doubles as the metric name the row's own series is written under, a
/// metric name being an opaque key — the bucket's values in timestamp order, the statistic
/// that reduces them, and the one number that reduction must answer.
type UnboundedCase = (&'static str, &'static [f64], &'static str, f64);

/// RULE: every `MetricStat` reduces its bucket to the number that statistic names for
/// every series this store can hold, a stored `Value` of unbounded magnitude included, so
/// a `Complete` result never carries NOT A NUMBER.
///
/// `BatchPutMetrics` stores an unbounded `Value` as the number it is and refuses only a
/// `NaN`, so each reduction is read off its own definition as a magnitude grows without
/// bound: the mean follows whichever unbounded direction outnumbers the other and keeps the
/// finite share when they cancel, a population with one value or one constant value has no
/// spread whatever that value is, and any other bucket holding an unbounded value spreads
/// without bound. Finite rows are in the same table because the same reduction answers
/// them.
#[tokio::test]
async fn aggregating_unbounded_values_answers_a_number_for_every_statistic() {
    // Population variance of {1.0, 2.0, 6.0} about its mean 3.0: (4 + 1 + 9) / 3.
    let finite_spread = (14.0_f64 / 3.0).sqrt();
    let cases: &[UnboundedCase] = &[
        // StdDev: the spread of a population that is one value, or one value repeated.
        ("a lone unbounded value", &[f64::INFINITY], "StdDev", 0.0),
        (
            "a lone unbounded value below",
            &[f64::NEG_INFINITY],
            "StdDev",
            0.0,
        ),
        (
            "an unbounded value repeated",
            &[f64::INFINITY, f64::INFINITY],
            "StdDev",
            0.0,
        ),
        // StdDev: any other bucket holding an unbounded value has no finite spread.
        (
            "unbounded above against unbounded below",
            &[f64::INFINITY, f64::NEG_INFINITY],
            "StdDev",
            f64::INFINITY,
        ),
        (
            "an unbounded value beside a finite one",
            &[f64::INFINITY, 1.0],
            "StdDev",
            f64::INFINITY,
        ),
        // Avg: the direction that outnumbers the other decides the mean.
        ("the mean runs up", &[f64::INFINITY, 1.0], "Avg", f64::INFINITY),
        (
            "the mean runs down",
            &[f64::NEG_INFINITY, 1.0],
            "Avg",
            f64::NEG_INFINITY,
        ),
        (
            "the mean runs up on the count",
            &[f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY],
            "Avg",
            f64::INFINITY,
        ),
        // Avg: equal counts in both directions cancel, leaving the finite share.
        (
            "opposed unbounded values and nothing else",
            &[f64::INFINITY, f64::NEG_INFINITY],
            "Avg",
            0.0,
        ),
        (
            "opposed unbounded values over a finite point",
            &[f64::INFINITY, f64::NEG_INFINITY, 3.0],
            "Avg",
            1.0,
        ),
        // The order statistics, the count and the latest point take an unbounded value as
        // the number it is.
        (
            "the smallest value is unbounded below",
            &[f64::INFINITY, f64::NEG_INFINITY, 1.0],
            "Min",
            f64::NEG_INFINITY,
        ),
        (
            "the largest value is unbounded above",
            &[f64::INFINITY, f64::NEG_INFINITY, 1.0],
            "Max",
            f64::INFINITY,
        ),
        (
            "unbounded values are counted like any other",
            &[f64::INFINITY, f64::NEG_INFINITY],
            "Count",
            2.0,
        ),
        (
            "the latest point of the bucket is unbounded",
            &[1.0, f64::INFINITY],
            "Last",
            f64::INFINITY,
        ),
        // The mean of values at the top of the double range is in that range, even where
        // their sum is not.
        (
            "a mean at the top of the range",
            &[f64::MAX, f64::MAX],
            "Avg",
            f64::MAX,
        ),
        (
            "a mean whose own sum would not fit",
            &[f64::MAX, f64::MAX, -f64::MAX],
            "Avg",
            f64::MAX / 3.0,
        ),
        (
            "a constant bucket at the top of the range",
            &[f64::MAX, f64::MAX],
            "StdDev",
            0.0,
        ),
        (
            // The spread of {M, -M} about its mean 0 is M itself, which is in the range even
            // though the squared deviation it is read off is not.
            "a spread at the top of the range",
            &[f64::MAX, -f64::MAX],
            "StdDev",
            f64::MAX,
        ),
        // Finite rows, answered by the same reduction.
        ("a finite mean", &[1.0, 2.0, 6.0], "Avg", 3.0),
        ("a finite spread", &[1.0, 2.0, 6.0], "StdDev", finite_spread),
    ];

    // The same table under two groupings, which pins the rule in the REDUCTION rather than
    // in how the points were bucketed: one bucket per hour over points a second apart, and
    // one bucket per iteration over points that share a step.
    let groupings: [(&'static str, &'static str, &'static str, &'static str, i64); 2] = [
        (
            "per hour",
            "tc-unbounded-hour",
            "OneHour",
            "Timestamp",
            HOUR_BUCKETS[0],
        ),
        (
            "per iteration",
            "tc-unbounded-iteration",
            "IterationNumber",
            "IterationNumber",
            0,
        ),
    ];

    for (grouping, component, period, axis, bucket) in groupings {
        let router = emulator();
        for (case, values, stat, want) in cases {
            // Every point of a row lands in ONE bucket: a second apart inside the same
            // hour, or at the same iteration, each at a coordinate of its own.
            let points: Vec<RawPoint> = values
                .iter()
                .enumerate()
                .map(|(at, value)| {
                    let step = if axis == "IterationNumber" {
                        0
                    } else {
                        i32::try_from(at).expect("a row holds a handful of points")
                    };
                    (
                        *case,
                        T0 + i64::try_from(at).expect("a row holds a handful of points"),
                        Some(step),
                        *value,
                    )
                })
                .collect();
            let errors = put_metrics(&router, component, &points).await;
            assert!(
                errors.is_empty(),
                "case {grouping}/{case:?}: every value of the bucket is a measurement and \
                 is stored, got {errors:?}"
            );

            let query = Query::new(case, component).aggregate(stat, period, axis);
            let results = get_metrics(&router, &[query]).await;
            assert_result(
                &format!("{grouping}/{case} as {stat}"),
                &results[0],
                "Complete",
                &[bucket],
                &[*want],
            );
        }
    }
}

/// The points the write-order test uses: distinct iteration numbers, distinct values.
const ORDER_POINTS: [(i32, f64); 4] = [(0, 1.0), (1, 0.5), (2, 0.25), (3, 0.125)];

/// Every permutation of `0..items`, in a stable order. Used to enumerate write orders
/// exhaustively rather than sampling them.
fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut all = Vec::new();
    for (position, &head) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(position);
        for mut tail in permutations(&rest) {
            tail.insert(0, head);
            all.push(tail);
        }
    }
    all
}

/// The invariant behind the series order, over EVERY write order rather than a sample:
/// however a batch's points arrive, the series reads back once per iteration ascending
/// in x with the value that was written — and writing the identical batch again changes
/// nothing (idempotence).
#[tokio::test]
async fn every_write_order_reads_back_ascending_and_is_idempotent() {
    let expected_x: Vec<i64> = ORDER_POINTS
        .iter()
        .map(|(step, _)| i64::from(*step))
        .collect();
    let expected_values: Vec<f64> = ORDER_POINTS.iter().map(|(_, value)| *value).collect();

    let indices: Vec<usize> = (0..ORDER_POINTS.len()).collect();
    for (case, order) in permutations(&indices).iter().enumerate() {
        let router = emulator();
        let component = format!("tc-order-{case}");
        let batch: Vec<RawPoint> = order
            .iter()
            .map(|&at| {
                let (step, value) = ORDER_POINTS[at];
                ("loss", T0 + i64::from(step), Some(step), value)
            })
            .collect();
        let query = Query::new("loss", &component)
            .aggregate("Last", "IterationNumber", "IterationNumber");

        // Written twice: the second write is the idempotence half of the invariant.
        for attempt in 1..=2 {
            let errors = put_metrics(&router, &component, &batch).await;
            assert!(
                errors.is_empty(),
                "case {order:?}: write {attempt} of the same batch must be accepted whole, \
                 got {errors:?}"
            );
            let results = get_metrics(&router, std::slice::from_ref(&query)).await;
            let got = &results[0];
            assert!(
                got.x_axis_values
                    .as_deref()
                    .unwrap_or_default()
                    .windows(2)
                    .all(|pair| pair[0] < pair[1]),
                "case {order:?}: XAxisValues must ascend, got {:?}",
                got.x_axis_values
            );
            assert_result(
                &format!("write-order {order:?} attempt {attempt}"),
                got,
                "Complete",
                &expected_x,
                &expected_values,
            );
        }
    }
}
