// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `AWS/Lambda` series an invocation that arrives through a FUNCTION
//! URL is metered on, read back where a client reads it — the series CloudWatch answers for,
//! with CloudWatch mounted beside Lambda on one engine.
//!
//! The RULE asserted below: Lambda meters EVERY invocation it performs of a function on that
//! function's own `AWS/Lambda` series, whichever surface started it — one `Invocations` count,
//! one `Duration` measurement in `Milliseconds` carrying the time that run took, and one
//! `Errors` indicator in `Count` carrying 1 for a run that came to a function error and 0 for
//! one that returned — dimensioned on the function (`FunctionName`) and on the resource of it
//! the call addressed (`FunctionName` plus `Resource`). So a client can alarm on the latency
//! and the error rate of a function whatever invokes it.
//!
//! What the sibling suites pin and this one does not repeat: `invocation_outcome_series.rs`
//! asserts the rule over the `Invoke` and `InvokeWithResponseStream` surfaces and over the
//! throttles neither reached the function, `invocation_count_series.rs` over what
//! `Invocations` counts, and `invocation_duration_report_agreement.rs` over the agreement
//! between a published `Duration` and the run's own `REPORT` line. What only this suite
//! reaches is the FUNCTION URL data plane: a request that never names an operation of this
//! service at all, served by the endpoint `CreateFunctionUrlConfig` minted, which is an
//! invocation of the function and therefore metered as one.
//!
//! The rows differ along what the rule generalises over: what the handler does (returns,
//! sleeps for a measurable span, raises — which this plane answers as a bad gateway), how many
//! requests the row makes and how many of them fail, which of the published addressings the
//! client reached the endpoint by, and what the request carries (a bare `GET`, a `POST` with a
//! body). A closing case drives one function through BOTH the URL plane and `Invoke` and reads
//! the single series the two must share.
//!
//! Every namespace, metric, dimension and unit name here is written as a literal taken from
//! the AWS Lambda metric reference rather than read from this crate's constants, so a series
//! published under a name no AWS reader would ask for fails here instead of agreeing with
//! itself. Nothing waits on wall clock: the measurements are published by the run the request
//! is answered off, so each row reads them once its own requests have been answered, and the
//! only span any assertion is about is the one a row's own handler sleeps.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{make_activation_due, ROLE};
use crate::function_url_data_plane::{
    control, given_configured_url, grant, send, statement, Endpoint,
};
use crate::kinesis_event_source_delivery::cloudwatch;
use crate::synchronous_invoke_run::package;

/// The namespace Lambda's own metrics live in (the AWS Lambda metric reference).
const NAMESPACE: &str = "AWS/Lambda";

/// The names that reference gives the series under test, and the units it reports them in.
const INVOCATIONS_SERIES: &str = "Invocations";
/// See [`INVOCATIONS_SERIES`].
const DURATION_SERIES: &str = "Duration";
/// See [`INVOCATIONS_SERIES`].
const ERRORS_SERIES: &str = "Errors";
/// See [`INVOCATIONS_SERIES`].
const MILLISECONDS_UNIT: &str = "Milliseconds";
/// See [`INVOCATIONS_SERIES`].
const COUNT_UNIT: &str = "Count";

/// The dimensions that reference reports them per: the function, and the resource of it an
/// invocation addressed.
const FUNCTION_DIMENSION: &str = "FunctionName";
/// See [`FUNCTION_DIMENSION`].
const RESOURCE_DIMENSION: &str = "Resource";

/// The function every case creates and invokes.
const FUNCTION: &str = "url-metered";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function here is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `AuthType` the URLs below carry, and the action a caller of such a URL is granted:
/// `AuthType=NONE` bypasses IAM authentication, so the function's own resource-based policy
/// is what admits the anonymous caller.
const PUBLIC_AUTH_TYPE: &str = "NONE";
/// See [`PUBLIC_AUTH_TYPE`].
const INVOKE_URL_ACTION: &str = "lambda:InvokeFunctionUrl";

/// The emulator's own endpoint, which a client that resolves no AWS hostname reaches.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";

/// The `Period`, in seconds, every query reads at: the one-minute granularity Lambda's own
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a query's window reaches, in seconds. Wide on purpose: no
/// assertion here is about where in the window a measurement landed.
const WINDOW_SECS: f64 = 600.0;

/// How long the row that measures a span has its handler sleep, in milliseconds, and the floor
/// that row's `Duration` must therefore clear.
const SLEPT_MS: f64 = 400.0;

/// The `Timeout` every function here is given, in seconds.
///
/// Generous on purpose, because nothing here is about it: a handler stopped at this deadline
/// is reported as a run that ERRORED, so a tight value would turn a slow or loaded host into a
/// failure the handler never raised. Well inside the range the model allows the member
/// (`smithy_query(query="shape", shape="Timeout")`: 1 to 5400).
const GENEROUS_TIMEOUT_SECS: u32 = 300;

/// The `MemorySize` every function here is given, in MB.
const MEMORY_SIZE_MB: u32 = 512;

/// The handler every case runs. It branches on the path the request carried, which is
/// `rawPath` in the payload-format-2.0 event a function URL hands it, so one package serves
/// every row and what the series measures is that row's own run.
const SOURCE: &str = r#"
import time


def lambda_handler(event, context):
    path = event.get("rawPath") or "/"
    if path.endswith("/slow"):
        time.sleep(0.4)
        return {"statusCode": 200, "body": "slept"}
    if path.endswith("/raise"):
        raise RuntimeError("this invocation failed on purpose")
    return {"statusCode": 200, "body": "served"}
"#;

/// Lambda and CloudWatch on one engine: the series Lambda publishes is CloudWatch's, so it is
/// read in-process through CloudWatch's own surface — the shape `core/router/src/lib.rs`
/// mounts them in.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
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
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Create the function every case invokes, bring it to `Active`, give it a public function URL
/// and answer the endpoint that URL published.
async fn given_function_url(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
) -> Endpoint {
    let created = control(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": GENEROUS_TIMEOUT_SECS,
            "MemorySize": MEMORY_SIZE_MB,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction must succeed, got {}",
        created.summary()
    );
    // The activation the CLOCK owes this function, brought forward instead of waited out.
    make_activation_due(engine);
    let endpoint =
        given_configured_url(router, FUNCTION, None, PUBLIC_AUTH_TYPE, &Value::Null).await;
    grant(
        router,
        FUNCTION,
        None,
        &statement("public-url", INVOKE_URL_ACTION, "*", PUBLIC_AUTH_TYPE),
    )
    .await;
    endpoint
}

/// Which of the published addressings a request reaches the endpoint by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressing {
    /// The host AWS publishes the URL at.
    HostStyle,
    /// The url id in the path, on the emulator's own endpoint — the addressing a client that
    /// resolves no AWS hostname reaches the same URL by.
    LocalPath,
}

/// One request a row makes to the function URL.
#[derive(Clone, Copy, Debug)]
struct Request {
    addressing: Addressing,
    method: &'static str,
    /// The path, which is what the handler branches on.
    path: &'static str,
    /// The request body, empty for a request that carries none.
    body: &'static str,
    /// The HTTP status the endpoint must answer: `200` for a handler that described a
    /// response, and `502` for a run that produced none at all.
    status: u16,
}

/// Make one of a row's requests to `endpoint`.
async fn request(
    router: &MultiServiceRouter,
    endpoint: &Endpoint,
    case: &str,
    index: usize,
    made: &Request,
) {
    let (host, uri) = match made.addressing {
        Addressing::HostStyle => (endpoint.host.clone(), made.path.to_string()),
        Addressing::LocalPath => (
            OWN_ENDPOINT.to_string(),
            format!("/_aws/lambda-url/{}{}", endpoint.url_id, made.path),
        ),
    };
    let headers: &[(&str, &str)] = if made.body.is_empty() {
        &[]
    } else {
        &[("content-type", "application/json")]
    };
    let answer = send(
        router,
        made.method,
        &host,
        &uri,
        made.body.as_bytes(),
        headers,
    )
    .await;
    assert_eq!(
        answer.status, made.status,
        "case {case:?}: request {index} of {made:?} must be answered {}, got {}",
        made.status,
        answer.summary()
    );
}

/// The dimension set of the series reported per function.
///
/// Shared with the suites that read the same series for requests of their own, beside
/// [`datapoints`], so every reader names a dimension the one way.
pub(crate) fn function_dimensions(function: &str) -> Value {
    json!([{ "Name": FUNCTION_DIMENSION, "Value": function }])
}

/// The dimension set of the series reported per invoked resource: the function itself, for a
/// URL attached to the unqualified function.
pub(crate) fn resource_dimensions(function: &str) -> Value {
    json!([
        { "Name": FUNCTION_DIMENSION, "Value": function },
        { "Name": RESOURCE_DIMENSION, "Value": function },
    ])
}

/// The `Datapoints` `GetMetricStatistics` answers for the series `metric` and `dimensions`
/// identify, selected by the unit the AWS Lambda metric reference gives that series.
///
/// Shared with the suites that read the same series for requests of their own, so every reader
/// asks the same question of CloudWatch.
pub(crate) async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    unit: &str,
    dimensions: &Value,
) -> Vec<Value> {
    let now = now_secs();
    let (status, stats) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount", "Minimum", "Maximum"],
            "Unit": unit,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics({metric}, {dimensions}) answered {status} {stats}"
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

/// The smallest `Minimum` any datapoint of a series reports, or `None` for a series with no
/// datapoint at all.
fn smallest(points: &[Value]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point["Minimum"].as_f64())
        .min_by(f64::total_cmp)
}

/// One row of the table: the requests it makes to the function URL, and what the three series
/// must then report about them.
struct Row {
    case: &'static str,
    requests: &'static [Request],
    /// What `Invocations` must sum to: the requests of the row that reached the function.
    invocation_sum: f64,
    /// How many measurements `Duration` and `Errors` must each hold: one per invocation that
    /// ran.
    samples: f64,
    /// The floor every `Duration` datapoint must clear, in milliseconds.
    least_duration_ms: f64,
    /// What `Errors` must sum to: the runs of the row that came to a function error.
    error_sum: f64,
}

/// The rows. Every expectation is written out rather than computed from the requests, so a
/// publisher that counted requests and one that counted runs cannot both pass.
const ROWS: &[Row] = &[
    Row {
        case: "one request whose handler describes a response",
        requests: &[Request {
            addressing: Addressing::HostStyle,
            method: "GET",
            path: "/return",
            body: "",
            status: 200,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        least_duration_ms: 0.0,
        error_sum: 0.0,
    },
    Row {
        case: "a run that takes a measurable span",
        requests: &[Request {
            addressing: Addressing::HostStyle,
            method: "GET",
            path: "/slow",
            body: "",
            status: 200,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        least_duration_ms: SLEPT_MS,
        error_sum: 0.0,
    },
    Row {
        case: "a handler that raises, which this plane answers as a bad gateway",
        requests: &[Request {
            addressing: Addressing::HostStyle,
            method: "GET",
            path: "/raise",
            body: "",
            status: 502,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        least_duration_ms: 0.0,
        error_sum: 1.0,
    },
    Row {
        case: "three requests carrying bodies and methods of their own, one of them failing",
        requests: &[
            Request {
                addressing: Addressing::HostStyle,
                method: "POST",
                path: "/return",
                body: r#"{"order":"a"}"#,
                status: 200,
            },
            Request {
                addressing: Addressing::HostStyle,
                method: "GET",
                path: "/raise",
                body: "",
                status: 502,
            },
            Request {
                addressing: Addressing::HostStyle,
                method: "PUT",
                path: "/return",
                body: r#"{"order":"b"}"#,
                status: 200,
            },
        ],
        invocation_sum: 3.0,
        samples: 3.0,
        least_duration_ms: 0.0,
        error_sum: 1.0,
    },
    Row {
        case: "the same endpoint reached by the addressing a local client writes",
        requests: &[Request {
            addressing: Addressing::LocalPath,
            method: "GET",
            path: "/return",
            body: "",
            status: 200,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        least_duration_ms: 0.0,
        error_sum: 0.0,
    },
    Row {
        case: "two addressings of one endpoint, metered on the one function",
        requests: &[
            Request {
                addressing: Addressing::HostStyle,
                method: "GET",
                path: "/return",
                body: "",
                status: 200,
            },
            Request {
                addressing: Addressing::LocalPath,
                method: "GET",
                path: "/raise",
                body: "",
                status: 502,
            },
        ],
        invocation_sum: 2.0,
        samples: 2.0,
        least_duration_ms: 0.0,
        error_sum: 1.0,
    },
];

/// RULE: an invocation that arrives through a function URL is metered on the invoked
/// function's own `AWS/Lambda` series exactly as any other invocation is — one `Invocations`
/// count, one `Duration` measurement carrying the time the run took, and one `Errors`
/// indicator of what it came to.
#[tokio::test]
async fn every_function_url_request_is_metered_on_the_functions_own_series() {
    for row in ROWS {
        // One account per row, so the only invocations in the window are this row's.
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);
        let case = row.case;
        let endpoint = given_function_url(&router, &engine, case).await;

        for (index, made) in row.requests.iter().enumerate() {
            request(&router, &endpoint, case, index, made).await;
        }

        for dimensions in [function_dimensions(FUNCTION), resource_dimensions(FUNCTION)] {
            let invocations =
                datapoints(&router, case, INVOCATIONS_SERIES, COUNT_UNIT, &dimensions).await;
            let counted = total(&invocations, "Sum");
            assert_eq!(
                counted, row.invocation_sum,
                "case {case:?}: {INVOCATIONS_SERIES} of {dimensions} must sum to {}, got \
                 {counted} over {invocations:?}",
                row.invocation_sum
            );

            let durations =
                datapoints(&router, case, DURATION_SERIES, MILLISECONDS_UNIT, &dimensions).await;
            let measured = total(&durations, "SampleCount");
            assert_eq!(
                measured, row.samples,
                "case {case:?}: {DURATION_SERIES} of {dimensions} must hold {} measurement(s), \
                 got {measured} over {durations:?}",
                row.samples
            );
            let least = smallest(&durations);
            assert!(
                least.is_some_and(|least| least >= row.least_duration_ms),
                "case {case:?}: every {DURATION_SERIES} datapoint of {dimensions} must be at \
                 least {} ms, got {least:?} over {durations:?}",
                row.least_duration_ms
            );
            assert!(
                durations
                    .iter()
                    .all(|point| point["Unit"].as_str() == Some(MILLISECONDS_UNIT)),
                "case {case:?}: every {DURATION_SERIES} datapoint of {dimensions} must carry \
                 the unit {MILLISECONDS_UNIT:?}, got {durations:?}"
            );

            // `Errors` is the binary indicator of what one invocation came to, so every run
            // of the row reports on it — 1 for a run that failed and 0 for one that returned.
            // The SUM is therefore the failures of the row and the SAMPLE COUNT is its runs,
            // which is what tells a function that failed nothing from one nobody invoked.
            let errors = datapoints(&router, case, ERRORS_SERIES, COUNT_UNIT, &dimensions).await;
            let failed = total(&errors, "Sum");
            assert_eq!(
                failed, row.error_sum,
                "case {case:?}: {ERRORS_SERIES} of {dimensions} must sum to {}, got {failed} \
                 over {errors:?}",
                row.error_sum
            );
            let reported = total(&errors, "SampleCount");
            assert_eq!(
                reported, row.samples,
                "case {case:?}: {ERRORS_SERIES} of {dimensions} must report on each of the {} \
                 run(s), got {reported} over {errors:?}",
                row.samples
            );
        }

        // Reading the series changes nothing a later reader sees.
        let again = datapoints(
            &router,
            case,
            INVOCATIONS_SERIES,
            COUNT_UNIT,
            &function_dimensions(FUNCTION),
        )
        .await;
        let counted_again = total(&again, "Sum");
        assert_eq!(
            counted_again, row.invocation_sum,
            "case {case:?}: a second read of {INVOCATIONS_SERIES} must report the same {}, got \
             {counted_again} over {again:?}",
            row.invocation_sum
        );
    }
}

/// RULE, on the identity of the series rather than its values: the series is the FUNCTION's,
/// so invocations that arrive on different surfaces accumulate on one series instead of each
/// surface publishing a series of its own.
#[tokio::test]
async fn the_url_plane_and_invoke_meter_one_function_on_one_series() {
    let case = "one request through the URL and one through Invoke";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let endpoint = given_function_url(&router, &engine, case).await;

    request(
        &router,
        &endpoint,
        case,
        0,
        &Request {
            addressing: Addressing::HostStyle,
            method: "GET",
            path: "/return",
            body: "",
            status: 200,
        },
    )
    .await;
    let invoked = control(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({ "rawPath": "/return" }),
    )
    .await;
    assert_eq!(
        invoked.status,
        200,
        "case {case:?}: Invoke must answer the run, got {}",
        invoked.summary()
    );

    let dimensions = function_dimensions(FUNCTION);
    let invocations =
        datapoints(&router, case, INVOCATIONS_SERIES, COUNT_UNIT, &dimensions).await;
    let counted = total(&invocations, "Sum");
    assert_eq!(
        counted, 2.0,
        "case {case:?}: {INVOCATIONS_SERIES} of {dimensions} must sum to 2, got {counted} over \
         {invocations:?}"
    );
    let durations =
        datapoints(&router, case, DURATION_SERIES, MILLISECONDS_UNIT, &dimensions).await;
    let measured = total(&durations, "SampleCount");
    assert_eq!(
        measured, 2.0,
        "case {case:?}: {DURATION_SERIES} of {dimensions} must hold a measurement of each run, \
         got {measured} over {durations:?}"
    );
    let errors = datapoints(&router, case, ERRORS_SERIES, COUNT_UNIT, &dimensions).await;
    let reported = total(&errors, "SampleCount");
    assert_eq!(
        reported, 2.0,
        "case {case:?}: {ERRORS_SERIES} of {dimensions} must report on each run, got {reported} \
         over {errors:?}"
    );
    let failed = total(&errors, "Sum");
    assert_eq!(
        failed, 0.0,
        "case {case:?}: {ERRORS_SERIES} of {dimensions} must sum to 0 when neither run failed, \
         got {failed} over {errors:?}"
    );
}
