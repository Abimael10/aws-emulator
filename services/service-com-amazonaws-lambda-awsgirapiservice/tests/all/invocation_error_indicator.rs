// SPDX-License-Identifier: CC0-1.0

//! What the `Errors` series says about a function whose invocations FAILED NOTHING, read back
//! where a client reads it — the `AWS/Lambda` series CloudWatch answers for, with CloudWatch
//! mounted beside Lambda on one engine.
//!
//! The RULE asserted over the table below: `Errors` is a binary indicator of ONE invocation's
//! outcome, so Lambda publishes exactly one `Errors` datapoint per invocation that RAN — 1 for a
//! run that came to a function error and 0 for one that returned. So, over any window of a
//! function's traffic, `Sum` is the failures of that window, `SampleCount` is its invocations and
//! `Average` is its error RATE; and a function that was invoked at all has the series while a
//! function nobody invoked has no datapoint on it at all, which is what tells "this function
//! failed nothing" from "this function never ran".
//!
//! The rows differ along what the rule generalises over: how many invocations are made and how
//! many of them fail (none, some, all), so the expected sum, sample count and rate of every row
//! are computed from the row rather than taken from a recording. The dimension sets are both the
//! ones Lambda reports an invocation on — the function's own and the invoked resource's.
//!
//! Every metric, dimension and unit name here is written as a literal, taken from the AWS Lambda
//! metric reference rather than from this crate's constants, so a series published under a name
//! no AWS reader would ask for fails here instead of agreeing with itself. The harness is the
//! Lambda-side one of `function_lifecycle.rs`, the deployment archive `synchronous_invoke_run.rs`'s
//! and the CloudWatch call `kinesis_event_source_delivery.rs`'s.
//!
//! Every wait here is for a CONDITION — the series holding what the invocations owe it — looked at
//! again until it holds or a generous ceiling passes, so how fast the host runs decides nothing
//! asserted here.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{make_activation_due, send, ROLE};
use crate::kinesis_event_source_delivery::cloudwatch;
use crate::synchronous_invoke_run::package;

/// The namespace Lambda's own metrics live in, the series under test and the unit it is
/// reported in (the AWS Lambda metric reference).
pub(crate) const NAMESPACE: &str = "AWS/Lambda";
pub(crate) const ERRORS_SERIES: &str = "Errors";
pub(crate) const COUNT_UNIT: &str = "Count";

/// The dimensions that reference reports it per: the function, and the resource of it an
/// invocation addressed.
const FUNCTION_DIMENSION: &str = "FunctionName";
/// See [`FUNCTION_DIMENSION`].
const RESOURCE_DIMENSION: &str = "Resource";

/// The module and entry point the deployment package holds its handler at.
const HANDLER: &str = "lambda_function.lambda_handler";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `Period`, in seconds, every query reads at: the one-minute granularity Lambda's own
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a query's window reaches, in seconds — wide enough that every
/// invocation of a row falls inside it whatever the phase of the clock.
pub(crate) const WINDOW_SECS: f64 = 600.0;

/// The `Timeout` every function here is given, in seconds. Generous on purpose: no assertion
/// here is about the timeout, and a run stopped at it would be reported as a failure the handler
/// never raised.
const TIMEOUT_SECS: u32 = 300;

/// How long the series is waited for after the invocations, and how often it is looked at. The
/// publication happens off the response path, so the wait is for the datapoints having arrived;
/// the ceiling is generous because only a publication that never happens may fail a row.
pub(crate) const PUBLICATION_BUDGET: Duration = Duration::from_secs(60);
/// See [`PUBLICATION_BUDGET`].
pub(crate) const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The handler every row runs: it fails the way the event asks, or returns a value.
const SOURCE: &str = r#"
def lambda_handler(event, context):
    if event.get("raises"):
        raise RuntimeError("this invocation failed on purpose")
    return {"ok": True}
"#;

/// Lambda and CloudWatch on one engine: the series Lambda publishes is CloudWatch's, so it is
/// read through CloudWatch's own surface — the shape `core/router/src/lib.rs` mounts them in.
pub(crate) fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// Now, in the epoch seconds a stored datapoint is stamped with.
pub(crate) fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Create the function `name` and bring it to `Active`, which is the state an invocation needs.
pub(crate) async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
) {
    let created = send(
        router,
        "POST",
        FUNCTIONS_URI,
        "application/json",
        serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": TIMEOUT_SECS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(SOURCE)),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(engine);
}

/// One synchronous invocation of `name`, whose handler raises when `raises`.
pub(crate) async fn invoke(router: &MultiServiceRouter, case: &str, name: &str, raises: bool) {
    let answer = send(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        "application/json",
        json!({ "raises": raises }).to_string().into_bytes(),
        &[],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the invocation of {name:?} must be answered HTTP 200, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let reported = answer.header("x-amz-function-error");
    assert_eq!(
        reported.is_some(),
        raises,
        "case {case:?}: the invocation of {name:?} must report a function error only when its \
         handler raised, got {reported:?}"
    );
}

/// The dimension sets Lambda reports an invocation of `function` on: the function's own, and the
/// resource the unqualified call addressed.
pub(crate) fn dimension_sets(function: &str) -> Vec<Value> {
    vec![
        json!([{ "Name": FUNCTION_DIMENSION, "Value": function }]),
        json!([
            { "Name": FUNCTION_DIMENSION, "Value": function },
            { "Name": RESOURCE_DIMENSION, "Value": function },
        ]),
    ]
}

/// The `Datapoints` `GetMetricStatistics` answers for the `Errors` series of `dimensions`.
async fn error_datapoints(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &Value,
) -> Vec<Value> {
    let now = now_secs();
    let (status, stats) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": ERRORS_SERIES,
            "Dimensions": dimensions,
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount", "Average", "Maximum"],
            "Unit": COUNT_UNIT,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics({ERRORS_SERIES}, {dimensions}) answered {status} \
         {stats}"
    );
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The total of one statistic over every datapoint of a series.
pub(crate) fn total(points: &[Value], statistic: &str) -> f64 {
    points
        .iter()
        .filter_map(|point| point[statistic].as_f64())
        .sum()
}

/// Look at the `Errors` series of `dimensions` until it reports on `wanted` invocations, and
/// answer what it held last. Driven off what has ARRIVED rather than off a span of wall clock.
async fn error_datapoints_reporting_on(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &Value,
    wanted: f64,
) -> Vec<Value> {
    let deadline = Instant::now() + PUBLICATION_BUDGET;
    loop {
        let points = error_datapoints(router, case, dimensions).await;
        if total(&points, "SampleCount") >= wanted || Instant::now() >= deadline {
            return points;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Whether `ListMetrics` discovers an `Errors` series for `function` — the read a client makes
/// before it alarms on one.
async fn errors_discovered_for(router: &MultiServiceRouter, case: &str, function: &str) -> bool {
    let (status, listed) =
        cloudwatch(router, "ListMetrics", &json!({ "Namespace": NAMESPACE })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListMetrics answered {status} {listed}"
    );
    listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|series| {
            series["MetricName"] == json!(ERRORS_SERIES)
                && series["Dimensions"]
                    .as_array()
                    .is_some_and(|dimensions| {
                        dimensions.iter().any(|dimension| {
                            dimension["Name"] == json!(FUNCTION_DIMENSION)
                                && dimension["Value"] == json!(function)
                        })
                    })
        })
}

/// One row: the invocations it makes, each saying whether its handler raises.
struct Row {
    case: &'static str,
    runs: &'static [bool],
}

/// The rows: a single healthy invocation, several healthy ones, a mixture, and traffic in which
/// every invocation fails.
const ROWS: &[Row] = &[
    Row {
        case: "one invocation that returns",
        runs: &[false],
    },
    Row {
        case: "three invocations, none of them failing",
        runs: &[false, false, false],
    },
    Row {
        case: "four invocations, one of them failing",
        runs: &[true, false, false, false],
    },
    Row {
        case: "two invocations, both of them failing",
        runs: &[true, true],
    },
];

/// RULE: every invocation that ran publishes one `Errors` indicator — 1 for a run that failed and
/// 0 for one that returned — so a window of a function's traffic reads its failures as `Sum`, its
/// invocations as `SampleCount` and its error rate as `Average`.
#[tokio::test]
async fn every_invocation_publishes_its_error_indicator() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);
        let function = format!("error-indicator-{index}");
        given_function(&router, &engine, case, &function).await;

        for raises in row.runs {
            invoke(&router, case, &function, *raises).await;
        }

        #[allow(clippy::cast_precision_loss)]
        let invocations = row.runs.len() as f64;
        #[allow(clippy::cast_precision_loss)]
        let failures = row.runs.iter().filter(|raises| **raises).count() as f64;
        let rate = failures / invocations;

        for dimensions in dimension_sets(&function) {
            let points =
                error_datapoints_reporting_on(&router, case, &dimensions, invocations).await;
            let reported_runs = total(&points, "SampleCount");
            assert_eq!(
                reported_runs, invocations,
                "case {case:?}: {ERRORS_SERIES} of {dimensions} must report on each of the \
                 {invocations} invocation(s), got {reported_runs} over {points:?}"
            );
            let sum = total(&points, "Sum");
            assert_eq!(
                sum, failures,
                "case {case:?}: {ERRORS_SERIES} of {dimensions} must sum to the {failures} \
                 failure(s), got {sum} over {points:?}"
            );
            // The rate the window reports: the failures over the invocations of it. Every
            // invocation of a row lands in the same period here, so the average of the one
            // datapoint is the rate; a row's datapoints are averaged over their sample counts
            // in general, which is why the sum and the sample count are asserted above.
            let averaged = points
                .iter()
                .filter_map(|point| {
                    let average = point["Average"].as_f64()?;
                    let samples = point["SampleCount"].as_f64()?;
                    Some(average * samples)
                })
                .sum::<f64>()
                / invocations;
            assert!(
                (averaged - rate).abs() < 1e-9,
                "case {case:?}: {ERRORS_SERIES} of {dimensions} must report an error rate of \
                 {rate}, got {averaged} over {points:?}"
            );
            let highest = points
                .iter()
                .filter_map(|point| point["Maximum"].as_f64())
                .max_by(f64::total_cmp);
            let expected_highest = if failures > 0.0 { 1.0 } else { 0.0 };
            assert_eq!(
                highest,
                Some(expected_highest),
                "case {case:?}: the indicator is binary, so the largest {ERRORS_SERIES} \
                 datapoint of {dimensions} must be {expected_highest}, got {highest:?} over \
                 {points:?}"
            );
        }

        // The series is DISCOVERABLE for a function that was invoked at all, however its
        // invocations went: that is the read a client makes before it alarms on the series.
        let discovered = errors_discovered_for(&router, case, &function).await;
        assert!(
            discovered,
            "case {case:?}: ListMetrics must discover the {ERRORS_SERIES} series of {function:?}"
        );
    }
}

/// RULE (the contrast that gives the zero its meaning): a function nobody invoked publishes NO
/// `Errors` datapoint at all, so an explicit zero — which only an invocation can publish — tells
/// a client that the function ran and failed nothing.
#[tokio::test]
async fn a_function_that_was_never_invoked_publishes_no_error_indicator() {
    let case = "a function created and never invoked";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let function = "error-indicator-idle";
    given_function(&router, &engine, case, function).await;

    for dimensions in dimension_sets(function) {
        let points = error_datapoints(&router, case, &dimensions).await;
        assert!(
            points.is_empty(),
            "case {case:?}: nothing ran, so {ERRORS_SERIES} of {dimensions} must hold no \
             datapoint at all, got {points:?}"
        );
    }
    let discovered = errors_discovered_for(&router, case, function).await;
    assert!(
        !discovered,
        "case {case:?}: ListMetrics must discover no {ERRORS_SERIES} series for a function that \
         was never invoked"
    );

    // And one invocation is all it takes for the series to exist, reading zero: the absence
    // above is the lack of traffic and not the harness's.
    invoke(&router, case, function, false).await;
    for dimensions in dimension_sets(function) {
        let points = error_datapoints_reporting_on(&router, case, &dimensions, 1.0).await;
        assert_eq!(
            total(&points, "SampleCount"),
            1.0,
            "case {case:?}: the one invocation must report on {ERRORS_SERIES} of {dimensions}, \
             got {points:?}"
        );
        assert_eq!(
            total(&points, "Sum"),
            0.0,
            "case {case:?}: the invocation returned, so {ERRORS_SERIES} of {dimensions} must \
             read zero, got {points:?}"
        );
    }
}
