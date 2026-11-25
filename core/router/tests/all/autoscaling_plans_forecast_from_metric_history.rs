// SPDX-License-Identifier: CC0-1.0

//! A predictive-scaling FORECAST is computed from the load metric's CloudWatch history,
//! driven through the WHOLE assembled router.
//!
//! RULE: `GetScalingPlanResourceForecastData` answers "predicted values, or data points, that
//! are calculated using historical data points from a specified CloudWatch load metric", and
//! its output's `Datapoints` member is `@required` — so a resource whose load metric nothing
//! has published carries that member as the EMPTY list, and a resource whose metric HAS been
//! published is forecast from what was published: the load forecast of an hour is the load
//! that metric's history holds for the same hour of day.
//!
//! Why this suite is HERE rather than beside the service's own: the history belongs to
//! CloudWatch, which is where a client publishes it with `PutMetricData`, so the rule can only
//! be driven end to end on an emulator where CloudWatch is mounted beside AWS Auto Scaling —
//! which is what `build_router` assembles. The service's own suites drive the same rule
//! against the shared directory those two services hand the observations over through.
//!
//! The rows span what the rule generalises over: the hour of day each observation falls in,
//! whether a metric was published at all, and whether what was published belongs to the metric
//! the scaling instruction names or to another one.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every call below is signed for.
const REGION: &str = "us-east-1";

/// CloudWatch's signing name and the service shape its `X-Amz-Target` names an operation under.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// AWS Auto Scaling's signing name and the same for its own service shape.
const PLANS_SIGNING_NAME: &str = "autoscaling-plans";
const PLANS_TARGET_PREFIX: &str = "AnyScaleScalingPlannerFrontendService";

/// The granularity forecast data exists at: "the granularity period that is used in the
/// forecast (60 minutes)".
const FORECAST_INTERVAL_SECONDS: i64 = 3_600;

/// Hours in a day, for the daily cycle the published history carries.
const HOURS_PER_DAY: i64 = 24;

/// The resource every plan here scales, and the group name inside its `ResourceId` — "a prefix
/// (autoScalingGroup) followed by the name of a specified Auto Scaling group".
const GROUP: &str = "forecast-group";

/// The metric the scaling instruction names as its load metric: a namespace of the client's
/// own, because `PutMetricData` refuses an `AWS/` namespace ("you should not specify a
/// namespace that begins with AWS/").
const LOAD_NAMESPACE: &str = "Workload";
const LOAD_METRIC: &str = "TotalLoad";

/// Another metric in the same namespace: published history that belongs to IT is no history
/// for the metric above.
const OTHER_METRIC: &str = "OtherLoad";

/// The target value the instruction tracks its scaling metric at, which is the load one unit
/// of capacity carries — so the capacity forecast is the load forecast divided by it.
const TARGET_VALUE: f64 = 50.0;

/// How close two doubles have to be to count as the same value.
const TOLERANCE: f64 = 1e-9;

/// The `Authorization` header a call carries while the router authenticates nothing: the
/// credential scope, which names the target service and its region.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `X-Amz-Target` RPC call, answered as `(status, parsed body)`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CloudWatch call (`awsJson1_0`) that must answer 200.
async fn cloudwatch(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
    let (status, parsed) = rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
}

/// One AWS Auto Scaling call (`awsJson1_1`) that must answer 200.
async fn plans(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        PLANS_SIGNING_NAME,
        PLANS_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from autoscaling-plans {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// The instant, in whole epoch seconds, this run is at.
fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_secs(),
    )
    .expect("an instant inside the range of a signed count")
}

/// The start of the forecast interval containing now.
fn current_interval() -> i64 {
    now_secs().div_euclid(FORECAST_INTERVAL_SECONDS) * FORECAST_INTERVAL_SECONDS
}

/// The hour of day, in UTC, of an instant given as whole epoch seconds.
fn hour_of_day(at_secs: i64) -> i64 {
    at_secs
        .div_euclid(FORECAST_INTERVAL_SECONDS)
        .rem_euclid(HOURS_PER_DAY)
}

/// The load the history carries at `hour` of the day: distinct per hour, so a forecast cannot
/// match it by accident.
fn published_load(hour: i64) -> f64 {
    100.0 + 10.0 * hour as f64
}

/// Publish one day of hourly history for `metric` through CloudWatch's own API.
///
/// One datapoint per hour of the day before now, each stamped with the interval it belongs to
/// — "If you do not specify a time stamp, the time stamp of the data point is the time that
/// `PutMetricData` was called" (`MetricDatum$Timestamp`), so the stamp is what places an
/// observation in its hour. Every stamp is inside the two weeks CloudWatch ingests.
async fn publish_day(router: &MultiServiceRouter, case: &str, metric: &str) {
    let current = current_interval();
    for hours_ago in 0..HOURS_PER_DAY {
        let at = current - hours_ago * FORECAST_INTERVAL_SECONDS;
        cloudwatch(
            router,
            case,
            "PutMetricData",
            &json!({
                "Namespace": LOAD_NAMESPACE,
                "MetricData": [{
                    "MetricName": metric,
                    "Dimensions": [{"Name": "AutoScalingGroupName", "Value": GROUP}],
                    "Timestamp": at,
                    "Value": published_load(hour_of_day(at)),
                }],
            }),
        )
        .await;
    }
}

/// One scaling instruction for the group, whose load metric is `metric` in [`LOAD_NAMESPACE`].
///
/// The dimensions are the ones the metric was published with:
/// `CustomizedLoadMetricSpecification$Dimensions` — "Conditional: If you published your metric
/// with dimensions, you must specify the same dimensions in your customized load metric
/// specification." `$Statistic`: "The only valid value is Sum."
fn instruction(metric: &str) -> Value {
    json!({
        "ServiceNamespace": "autoscaling",
        "ResourceId": format!("autoScalingGroup/{GROUP}"),
        "ScalableDimension": "autoscaling:autoScalingGroup:DesiredCapacity",
        "MinCapacity": 0,
        "MaxCapacity": 10,
        "TargetTrackingConfigurations": [{
            "PredefinedScalingMetricSpecification": {
                "PredefinedScalingMetricType": "ASGAverageCPUUtilization"
            },
            "TargetValue": TARGET_VALUE,
        }],
        "CustomizedLoadMetricSpecification": {
            "MetricName": metric,
            "Namespace": LOAD_NAMESPACE,
            "Dimensions": [{"Name": "AutoScalingGroupName", "Value": GROUP}],
            "Statistic": "Sum",
        },
        "PredictiveScalingMode": "ForecastOnly",
        "DisableDynamicScaling": true,
    })
}

/// Create the plan `name` covering the group, and wait for it to settle `Active`.
async fn active_plan(router: &MultiServiceRouter, case: &str, name: &str, metric: &str) {
    let created = plans(
        router,
        case,
        "CreateScalingPlan",
        &json!({
            "ScalingPlanName": name,
            "ApplicationSource": {
                "TagFilters": [{"Key": "forecast-history", "Values": [name]}]
            },
            "ScalingInstructions": [instruction(metric)],
        }),
    )
    .await;
    assert_eq!(
        created.get("ScalingPlanVersion"),
        Some(&json!(1)),
        "case {case:?}: expected the plan to be created at version 1, got {created}"
    );
    // A created plan reaches `Active` on the clock the service stored, so the status is POLLED
    // rather than waited out: each look is a read, and a read is what retires what the clock
    // has made due. Generous, and nothing is asserted about how long it takes.
    for _ in 0..600 {
        let described = plans(
            router,
            case,
            "DescribeScalingPlans",
            &json!({"ScalingPlanNames": [name]}),
        )
        .await;
        if described["ScalingPlans"][0]["StatusCode"] == json!("Active") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("case {case:?}: the plan never settled Active");
}

/// The `(timestamp in whole epoch seconds, value)` pairs one forecast read answers with, over a
/// window of whole forecast intervals around now.
async fn forecast(
    router: &MultiServiceRouter,
    case: &str,
    plan: &str,
    data_type: &str,
) -> Vec<(i64, f64)> {
    let start = current_interval() - 6 * FORECAST_INTERVAL_SECONDS;
    let body = plans(
        router,
        case,
        "GetScalingPlanResourceForecastData",
        &json!({
            "ScalingPlanName": plan,
            "ScalingPlanVersion": 1,
            "ServiceNamespace": "autoscaling",
            "ResourceId": format!("autoScalingGroup/{GROUP}"),
            "ScalableDimension": "autoscaling:autoScalingGroup:DesiredCapacity",
            "ForecastDataType": data_type,
            "StartTime": start,
            "EndTime": start + 12 * FORECAST_INTERVAL_SECONDS,
        }),
    )
    .await;
    let listed = body
        .get("Datapoints")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?} / {data_type}: expected the required Datapoints list, got {body}")
        })
        .clone();
    listed
        .iter()
        .map(|point| {
            let at = point["Timestamp"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?} / {data_type}: a data point needs a Timestamp, got {point}")
            });
            let value = point["Value"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?} / {data_type}: a data point needs a Value, got {point}")
            });
            (at as i64, value)
        })
        .collect()
}

/// `value` rounded up to a whole unit of capacity, which is how capacity is counted
/// (`ResourceCapacity` is an integer in the model).
fn whole_units(value: f64) -> f64 {
    let nearest = value.round();
    if (value - nearest).abs() <= TOLERANCE * nearest.abs().max(1.0) {
        nearest
    } else {
        value.ceil()
    }
}

/// Every modelled `ForecastDataType` (`smithy_query(query="shape",shape="ForecastDataType")`).
const FORECAST_DATA_TYPES: [&str; 4] = [
    "LoadForecast",
    "CapacityForecast",
    "ScheduledActionMinCapacity",
    "ScheduledActionMaxCapacity",
];

#[test]
fn a_forecast_is_issued_only_from_the_load_metrics_published_history() {
    std::thread::Builder::new()
        .name("forecast-from-metric-history".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                router.set_request_authentication(false);

                // A plan over a group nothing has published a load metric for: every forecast
                // data type answers the required member, empty.
                let cold = "cold-forecast";
                active_plan(&router, "a resource with no metric history", cold, LOAD_METRIC).await;
                for data_type in FORECAST_DATA_TYPES {
                    let got = forecast(
                        &router,
                        "a resource with no metric history",
                        cold,
                        data_type,
                    )
                    .await;
                    assert!(
                        got.is_empty(),
                        "case a resource with no metric history / {data_type}: expected no data \
                         point, got {got:?}"
                    );
                }

                // History published for ANOTHER metric of the same namespace is no history for
                // the metric the instruction names.
                publish_day(&router, "a history published for another metric", OTHER_METRIC).await;
                let got = forecast(
                    &router,
                    "a history published for another metric",
                    cold,
                    "LoadForecast",
                )
                .await;
                assert!(
                    got.is_empty(),
                    "case a history published for another metric: expected no forecast for the \
                     metric the instruction names, got {got:?}"
                );

                // The metric the instruction names, published through CloudWatch: the load
                // forecast of each interval is what that hour of day was observed carrying, and
                // the capacity forecast follows from it and the target value.
                publish_day(&router, "the metric the instruction names", LOAD_METRIC).await;
                let case = "a resource whose load metric was published";
                let load = forecast(&router, case, cold, "LoadForecast").await;
                assert!(
                    !load.is_empty(),
                    "case {case:?}: a published metric history is forecast from, got nothing"
                );
                for (at, value) in &load {
                    let expected = published_load(hour_of_day(*at));
                    assert!(
                        (value - expected).abs() <= TOLERANCE * expected.abs().max(1.0),
                        "case {case:?} at {at}: expected the published load {expected}, got {value}"
                    );
                }
                let capacity = forecast(&router, case, cold, "CapacityForecast").await;
                assert_eq!(
                    capacity.len(),
                    load.len(),
                    "case {case:?}: the capacity forecast covers the same intervals as the load \
                     forecast, got {capacity:?} against {load:?}"
                );
                for ((at, forecast_capacity), (_, observed)) in capacity.iter().zip(&load) {
                    let expected = whole_units(observed / TARGET_VALUE);
                    assert!(
                        (forecast_capacity - expected).abs()
                            <= TOLERANCE * expected.abs().max(1.0),
                        "case {case:?} at {at}: expected the capacity {expected} the published \
                         load implies, got {forecast_capacity}"
                    );
                }

                // The plan's `PredictiveScalingMode` is `ForecastOnly`, under which AWS Auto
                // Scaling "forecasts capacity but does not create any scheduled scaling
                // actions", so those two data types stay empty however much history is held.
                for data_type in ["ScheduledActionMinCapacity", "ScheduledActionMaxCapacity"] {
                    let got = forecast(&router, case, cold, data_type).await;
                    assert!(
                        got.is_empty(),
                        "case {case:?} / {data_type}: a forecast-only plan creates no scheduled \
                         scaling action, got {got:?}"
                    );
                }
            });
        })
        .expect("spawn the suite thread")
        .join()
        .expect("the suite thread completes");
}
