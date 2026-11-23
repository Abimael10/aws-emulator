// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a deployed HTTP api stage publishes about the requests its ROUTES
//! served.
//!
//! The rule under test, one level above the recorded case:
//!
//! > Every request a ROUTE of a deployed HTTP api stage served is counted in API Gateway's own
//! > `AWS/ApiGateway` namespace: one observation of `Count`, and one of each error counter — `4xx` for the
//! > client-error class and `5xx` for the server-error class — which is 1 for a request whose
//! > status is in that class and 0 for one that is not. The matched route is what attributes a
//! > request to the stage's series rather than the answer it ended in, so a throttled route's
//! > `429` is counted exactly like a backend's `200` and the status alone decides the counter —
//! > while a request NO route of the stage served, which the gateway answers itself out of the
//! > api's records (a path no route key matches), is counted in none of these series. The series
//! > are dimensioned by `ApiId` and by `ApiId` with `Stage`, and a route whose serving stage
//! > sets `DetailedMetricsEnabled` gets the same observations again under its own `Method` and
//! > `Resource` dimensions — a stage that asks for nothing publishes the stage-level series and
//! > no route's. Beside those counters, the stage publishes one `IntegrationLatency` sample per
//! > request whose route's BACKEND ANSWERED the invocation the gateway relayed to it, and none
//! > for a request answered without a backend's answer — so that series' `SampleCount` is how
//! > many invocations a backend answered rather than how many requests the stage served.
//!
//! Only a cross-service suite can pin this: the api, its stage and its route settings are this
//! service's, the answer a route is served with belongs to a function AWS Lambda holds and
//! runs, and the metric store the counters land in is CloudWatch's — all three on ONE engine,
//! as `core/router/src/lib.rs` mounts them. Every assertion reads the outcome back through
//! CloudWatch's own readers (`ListMetrics` and `GetMetricStatistics`), so an observation that
//! did not reach that store cannot pass.
//!
//! The rows differ along what the rule generalises over: the STATUS CLASS the stage answered
//! with (a backend's success, a throttled route, a backend that produced nothing), how MANY
//! requests were made, how many of them reached a path NO route matches (none, some beside
//! routed traffic, only those), which STAGE served them (a named one and the one that serves
//! the api's root), the ROUTE KEY's shape, and WHERE the detailed-metrics opt-in is stated (on
//! the route's own settings, on the stage's defaults beside a route that states only a
//! throttle, nowhere).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, given_api, given_function, lambda_integration, ECHO_EVENT,
};
use crate::websocket_handshake_data_plane::{send, Answer};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The namespace API Gateway publishes what its HTTP apis answered in.
const NAMESPACE: &str = "AWS/ApiGateway";

/// The counters and the measurements one answered request is observed in.
pub(crate) const COUNT: &str = "Count";
const CLIENT_ERRORS: &str = "4xx";
const SERVER_ERRORS: &str = "5xx";
pub(crate) const LATENCY: &str = "Latency";
const DATA_PROCESSED: &str = "DataProcessed";
pub(crate) const INTEGRATION_LATENCY: &str = "IntegrationLatency";

/// The dimensions those series are identified by.
pub(crate) const API_ID: &str = "ApiId";
pub(crate) const STAGE: &str = "Stage";
const METHOD: &str = "Method";
const RESOURCE: &str = "Resource";

/// A handler that RAISES, so the route's integration produces no response and the gateway
/// answers the server-error class for it.
const RAISES: &str = "def lambda_handler(event, context):\n    raise RuntimeError('no answer')\n";

/// The window every read below covers, in seconds, and the period it is read at: generous
/// enough on both sides that no case depends on where in a minute the suite runs.
const WINDOW_SECONDS: f64 = 900.0;
const AHEAD_SECONDS: f64 = 120.0;
const PERIOD_SECONDS: i64 = 60;

/// This service, AWS Lambda and CloudWatch on ONE engine, as `core/router/src/lib.rs` mounts
/// them.
pub(crate) fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One CloudWatch call over its `awsJson1_0` wire, answering its status and parsed body.
async fn monitoring(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let answer: Answer = crate::websocket_handshake_data_plane::send_with_body(
        router,
        &format!("monitoring.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            (
                "authorization",
                &format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/monitoring/\
                     aws4_request, SignedHeaders=host, Signature=0000"
                ),
            ),
            (
                "x-amz-target",
                &format!("GraniteServiceVersion20100801.{operation}"),
            ),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    (answer.status, answer.body)
}

/// Now, as the epoch seconds a CloudWatch read states its window in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// The `Sum` of every datapoint the series `metric`/`dimensions` holds in the window.
///
/// Summed over the datapoints rather than read off one, because a window boundary crossed
/// while a case is running splits its observations over two periods — and the whole of what a
/// counter says is its total.
pub(crate) async fn total(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &[(&str, String)],
) -> Option<f64> {
    aggregated(router, metric, dimensions, "Sum").await
}

/// How many OBSERVATIONS the series holds in the window: its `SampleCount`, added up the same
/// way, which for a measurement is the number of things measured.
pub(crate) async fn samples(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &[(&str, String)],
) -> Option<f64> {
    aggregated(router, metric, dimensions, "SampleCount").await
}

/// The `statistic` of every datapoint the series `metric`/`dimensions` holds in the window,
/// added up over the periods the window covers.
async fn aggregated(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &[(&str, String)],
    statistic: &str,
) -> Option<f64> {
    let now = now_secs();
    let (status, body) = monitoring(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions
                .iter()
                .map(|(name, value)| json!({ "Name": name, "Value": value }))
                .collect::<Vec<Value>>(),
            "StartTime": now - WINDOW_SECONDS,
            "EndTime": now + AHEAD_SECONDS,
            "Period": PERIOD_SECONDS,
            "Statistics": [statistic],
        }),
    )
    .await;
    if status != 200 {
        return None;
    }
    Some(
        body.get("Datapoints")
            .and_then(Value::as_array)
            .map(|points| {
                points
                    .iter()
                    .map(|point| {
                        point
                            .get(statistic)
                            .and_then(Value::as_f64)
                            .unwrap_or_default()
                    })
                    .sum()
            })
            .unwrap_or_default(),
    )
}

/// Every series this namespace holds for `api_id`, as `(metric name, its dimension pairs)`
/// with the pairs ordered by dimension name.
pub(crate) async fn series(
    router: &MultiServiceRouter,
    api_id: &str,
) -> Vec<(String, Vec<(String, String)>)> {
    let (status, body) = monitoring(router, "ListMetrics", json!({ "Namespace": NAMESPACE })).await;
    assert_eq!(status, 200, "ListMetrics answered {status} {body}");
    let mut held: Vec<(String, Vec<(String, String)>)> = body
        .get("Metrics")
        .and_then(Value::as_array)
        .map(|metrics| {
            metrics
                .iter()
                .map(|metric| {
                    let mut dimensions: Vec<(String, String)> = metric
                        .get("Dimensions")
                        .and_then(Value::as_array)
                        .map(|pairs| {
                            pairs
                                .iter()
                                .map(|pair| {
                                    (
                                        pair.get("Name")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default()
                                            .to_string(),
                                        pair.get("Value")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default()
                                            .to_string(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    dimensions.sort();
                    (
                        metric
                            .get("MetricName")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        dimensions,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    held.retain(|(_, dimensions)| {
        dimensions
            .iter()
            .any(|(name, value)| name == API_ID && value == api_id)
    });
    held.sort();
    held
}

/// One row: an api whose stage answers a run of requests, and what the series it publishes
/// then hold.
struct Row {
    case: &'static str,
    /// The stage that serves the traffic.
    stage_name: &'static str,
    /// The route the api holds, and the verb and path within the stage each request uses.
    route_key: &'static str,
    verb: &'static str,
    route_path: &'static str,
    /// The handler behind the route: one that answers, or one that raises.
    handler: &'static str,
    /// The throttle the stage states for the route, as `(rate, burst)`, or none at all.
    throttle: Option<(f64, i32)>,
    /// Where the detailed-metrics opt-in is stated: on the route's own settings, on the
    /// stage's defaults, or nowhere.
    detailed: Detailed,
    /// The status each request to the row's own route path, in order, is answered with.
    statuses: &'static [u16],
    /// How many of those requests' invocations the route's BACKEND ANSWERED, which is what the
    /// stage has an `IntegrationLatency` sample for: a request the stage refused before the
    /// backend was reached has none, and a function that ran and raised answered the
    /// invocation with Lambda's error document, so it has one.
    backend_answers: f64,
    /// Paths within the same stage that NO route of the api matches. Each is requested once,
    /// is answered by the gateway itself, and is counted in none of the stage's series.
    unrouted_paths: &'static [&'static str],
    /// The `Method`/`Resource` dimensions of the route series the row expects, or `None` for a
    /// row that expects none at all.
    route_series: Option<(&'static str, &'static str)>,
}

/// The status an HTTP api's endpoint answers a path no route of the served deployment matches
/// with: `404`, the answer this data plane gives for a resource it does not have.
const NO_ROUTE_STATUS: u16 = 404;

/// Where a stage states `DetailedMetricsEnabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detailed {
    /// Nowhere: the stage asks for no route's series.
    Off,
    /// On the settings of the route itself.
    OnTheRoute,
    /// On the stage's `DefaultRouteSettings`, which govern every route it states nothing for.
    OnTheStageDefaults,
}

/// The rows: every status class, both stage forms, both places the opt-in can be stated, and
/// traffic a route served beside traffic no route did.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one request a function served, on a named stage",
            stage_name: "live",
            route_key: "GET /served",
            verb: "GET",
            route_path: "/served",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::Off,
            statuses: &[200],
            backend_answers: 1.0,
            unrouted_paths: &[],
            route_series: None,
        },
        Row {
            case: "three requests a function served, on the stage that serves the api's root",
            stage_name: "$default",
            route_key: "GET /served",
            verb: "GET",
            route_path: "/served",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::Off,
            statuses: &[200, 200, 200],
            backend_answers: 3.0,
            unrouted_paths: &[],
            route_series: None,
        },
        Row {
            case: "only paths no route of the api matches, which the gateway answers itself \
                   and no series counts",
            stage_name: "live",
            route_key: "GET /served",
            verb: "GET",
            route_path: "/served",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::Off,
            statuses: &[],
            backend_answers: 0.0,
            unrouted_paths: &["/nothing-here", "/nothing/here/either", "/served/deeper"],
            route_series: None,
        },
        Row {
            case: "the recorded shape: requests a route served, beside requests to a path no \
                   route matches",
            stage_name: "busy",
            route_key: "GET /served",
            verb: "GET",
            route_path: "/served",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::Off,
            statuses: &[200, 200, 200, 200],
            backend_answers: 4.0,
            unrouted_paths: &["/nothing-here", "/also-nothing"],
            route_series: None,
        },
        Row {
            case: "a route the stage throttles to nothing, whose refusals are the 4xx class",
            stage_name: "live",
            route_key: "GET /metered",
            verb: "GET",
            route_path: "/metered",
            handler: ECHO_EVENT,
            throttle: Some((0.0, 1)),
            detailed: Detailed::Off,
            statuses: &[200, 429, 429, 429],
            backend_answers: 1.0,
            unrouted_paths: &[],
            route_series: None,
        },
        Row {
            case: "a route whose function produced no response, which is the 5xx class",
            stage_name: "live",
            route_key: "GET /broken",
            verb: "GET",
            route_path: "/broken",
            handler: RAISES,
            throttle: None,
            detailed: Detailed::Off,
            statuses: &[500],
            backend_answers: 1.0,
            unrouted_paths: &[],
            route_series: None,
        },
        Row {
            case: "a route whose own settings ask for detailed metrics",
            stage_name: "live",
            route_key: "GET /served",
            verb: "GET",
            route_path: "/served",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::OnTheRoute,
            statuses: &[200, 200],
            backend_answers: 2.0,
            unrouted_paths: &[],
            route_series: Some(("GET", "/served")),
        },
        Row {
            case: "a detailed-metrics route served beside a path no route matches, which \
                   neither series counts",
            stage_name: "live",
            route_key: "GET /served",
            verb: "GET",
            route_path: "/served",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::OnTheRoute,
            statuses: &[200, 200],
            backend_answers: 2.0,
            unrouted_paths: &["/nothing-here"],
            route_series: Some(("GET", "/served")),
        },
        Row {
            case: "a stage whose default route settings ask for them, on a route key carrying \
                   a path variable",
            stage_name: "live",
            route_key: "POST /orders/{id}",
            verb: "POST",
            route_path: "/orders/7",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::OnTheStageDefaults,
            statuses: &[200],
            backend_answers: 1.0,
            unrouted_paths: &[],
            route_series: Some(("POST", "/orders/{id}")),
        },
        Row {
            case: "a detailed-metrics stage whose route key answers every verb",
            stage_name: "live",
            route_key: "ANY /{proxy+}",
            verb: "DELETE",
            route_path: "/anything/at/all",
            handler: ECHO_EVENT,
            throttle: None,
            detailed: Detailed::OnTheRoute,
            statuses: &[200],
            backend_answers: 1.0,
            unrouted_paths: &[],
            route_series: Some(("ANY", "/{proxy+}")),
        },
        Row {
            case: "a route whose own settings state only a throttle, under a stage whose \
                   defaults ask for detailed metrics",
            stage_name: "live",
            route_key: "GET /metered",
            verb: "GET",
            route_path: "/metered",
            handler: ECHO_EVENT,
            throttle: Some((0.0, 1)),
            detailed: Detailed::OnTheStageDefaults,
            statuses: &[200, 429],
            backend_answers: 1.0,
            unrouted_paths: &[],
            route_series: Some(("GET", "/metered")),
        },
    ]
}

/// The stage this row creates: its name, auto-deploy, and the route settings its throttle and
/// its detailed-metrics opt-in are stated in.
fn stage_of(row: &Row) -> Value {
    let mut settings = json!({});
    if let Some((rate, burst)) = row.throttle {
        settings["throttlingRateLimit"] = json!(rate);
        settings["throttlingBurstLimit"] = json!(burst);
    }
    if row.detailed == Detailed::OnTheRoute {
        settings["detailedMetricsEnabled"] = json!(true);
    }
    let mut stage = json!({ "stageName": row.stage_name, "autoDeploy": true });
    if settings.as_object().is_some_and(|held| !held.is_empty()) {
        stage["routeSettings"] = json!({ row.route_key: settings });
    }
    if row.detailed == Detailed::OnTheStageDefaults {
        stage["defaultRouteSettings"] = json!({ "detailedMetricsEnabled": true });
    }
    stage
}

/// The path within the api's endpoint a request of this row is made at: the route path itself
/// on the stage that serves the api's root, and the stage name in front of it otherwise.
fn invoke_path(row: &Row) -> String {
    if row.stage_name == "$default" {
        return row.route_path.to_string();
    }
    format!("/{}{}", row.stage_name, row.route_path)
}

/// The same addressing for a path WITHIN the row's stage that no route of the api matches, so
/// the request reaches the stage and is answered by the gateway rather than by a route.
fn unrouted_path(row: &Row, path: &str) -> String {
    if row.stage_name == "$default" {
        return path.to_string();
    }
    format!("/{}{}", row.stage_name, path)
}

/// Every request a route of the stage served is counted, its status decides which counter, a
/// request no route served is counted nowhere, and the route series exist exactly for a route
/// the stage asked for them for.
#[tokio::test]
async fn every_routed_request_is_counted_in_the_series_its_status_belongs_to() {
    for row in rows() {
        let case = row.case;
        // One emulator per row, so each row's series hold exactly its own traffic.
        let (router, engine) = emulator();
        let function = given_function(&router, &engine, case, "metric-source", row.handler).await;
        let (api_id, host) = given_api(
            &router,
            case,
            lambda_integration(&function, "2.0"),
            row.route_key,
            stage_of(&row),
        )
        .await;

        let path = invoke_path(&row);
        for (attempt, expected) in row.statuses.iter().enumerate() {
            let answered = send(&router, &host, row.verb, &path, &[]).await;
            assert_eq!(
                answered.status, *expected,
                "case {case:?}: expected request {attempt} at {path:?} to be answered \
                 {expected}, got {}",
                answered.summary()
            );
        }
        // The traffic no route of the stage matches: served by the gateway itself, so the
        // stage answered it and no series of the api counts it.
        for unrouted in row.unrouted_paths {
            let path = unrouted_path(&row, unrouted);
            let answered = send(&router, &host, row.verb, &path, &[]).await;
            assert_eq!(
                answered.status, NO_ROUTE_STATUS,
                "case {case:?}: expected the request at {path:?}, which no route matches, to be \
                 answered {NO_ROUTE_STATUS}, got {}",
                answered.summary()
            );
        }

        let answered = row.statuses.len() as f64;
        let client_errors = row
            .statuses
            .iter()
            .filter(|status| (400..500).contains(*status))
            .count() as f64;
        let server_errors = row
            .statuses
            .iter()
            .filter(|status| (500..600).contains(*status))
            .count() as f64;

        // The stage-level series: the api alone, and the api with the stage that served. A row
        // whose routed traffic is empty expects NO series at all: nothing the stage routed, so
        // nothing to publish.
        let api_dimensions = vec![(API_ID, api_id.clone())];
        let stage_dimensions = vec![
            (API_ID, api_id.clone()),
            (STAGE, row.stage_name.to_string()),
        ];
        let mut expected_dimension_sets: Vec<Vec<(String, String)>> = if row.statuses.is_empty() {
            Vec::new()
        } else {
            vec![pairs(&api_dimensions), pairs(&stage_dimensions)]
        };
        let mut scopes = vec![
            ("the api's own series", api_dimensions),
            ("the stage's series", stage_dimensions.clone()),
        ];
        if let Some((method, resource)) = row.route_series {
            let route_dimensions = vec![
                (API_ID, api_id.clone()),
                (STAGE, row.stage_name.to_string()),
                (METHOD, method.to_string()),
                (RESOURCE, resource.to_string()),
            ];
            if !row.statuses.is_empty() {
                expected_dimension_sets.push(pairs(&route_dimensions));
            }
            scopes.push(("the route's own series", route_dimensions));
        }

        for (scope, dimensions) in &scopes {
            for (metric, expected) in [
                (COUNT, answered),
                (CLIENT_ERRORS, client_errors),
                (SERVER_ERRORS, server_errors),
            ] {
                let actual = total(&router, metric, dimensions).await;
                assert_eq!(
                    actual,
                    Some(expected),
                    "case {case:?}: expected {scope} to hold {expected} in {metric}, got \
                     {actual:?}"
                );
            }
            // The BACKEND's own latency is observed once per invocation a backend answered —
            // never for a request the stage answered without one — so the series' sample count
            // is that number and not how many requests the stage served.
            let actual = samples(&router, INTEGRATION_LATENCY, dimensions).await;
            assert_eq!(
                actual,
                Some(row.backend_answers),
                "case {case:?}: expected {scope} to hold {} {INTEGRATION_LATENCY} samples, got \
                 {actual:?}",
                row.backend_answers
            );
        }

        // WHICH series exist: the stage-level ones for a stage whose routes served something,
        // the route's only where the stage asked for it, every metric of a routed request under
        // each — and none at all for a stage no route of which served anything.
        let held = series(&router, &api_id).await;
        let mut dimension_sets: Vec<Vec<(String, String)>> =
            held.iter().map(|(_, pairs)| pairs.clone()).collect();
        dimension_sets.sort();
        dimension_sets.dedup();
        expected_dimension_sets.sort();
        assert_eq!(
            dimension_sets, expected_dimension_sets,
            "case {case:?}: expected exactly these dimension sets for the api, got {held:?}"
        );
        let mut metrics: Vec<String> = held.iter().map(|(metric, _)| metric.clone()).collect();
        metrics.sort();
        metrics.dedup();
        let mut expected_metrics = if row.statuses.is_empty() {
            Vec::new()
        } else {
            vec![
                COUNT.to_string(),
                CLIENT_ERRORS.to_string(),
                SERVER_ERRORS.to_string(),
                LATENCY.to_string(),
                DATA_PROCESSED.to_string(),
            ]
        };
        // The backend-latency series exists only where a backend answered something.
        if row.backend_answers > 0.0 {
            expected_metrics.push(INTEGRATION_LATENCY.to_string());
        }
        expected_metrics.sort();
        assert_eq!(
            metrics, expected_metrics,
            "case {case:?}: expected every metric of a routed request, got {held:?}"
        );
    }
}

/// A `REQUEST` authorizer's function that refuses every request it is asked about: the simple
/// response payload format version 2.0 admits ("`isAuthorized`"), set to a refusal.
const DENIES: &str = "def lambda_handler(event, context):\n    return {'isAuthorized': False}\n";

/// The status a request its route's authorizer refused is answered with.
const FORBIDDEN: u16 = 403;

/// The stage that serves the gated route below, and the route itself.
const GATED_STAGE: &str = "live";
const GATED_ROUTE_KEY: &str = "GET /orders";
const GATED_ROUTE_PATH: &str = "/orders";

/// How many refused requests the case makes: more than one, so a counter reading the class
/// cannot be confused with a flag.
const REFUSED_REQUESTS: usize = 3;

/// A request a route's AUTHORIZER refused is still traffic of the stage's route: it is counted
/// in `Count` and in the lowercase `4xx` series under the api's own dimensions, and it has NO
/// backend latency at all, because the refusal was decided before anything was relayed to the
/// route's integration.
#[tokio::test]
async fn a_request_the_authorizer_refused_is_counted_as_a_client_error_with_no_backend_latency() {
    let case = "a route whose REQUEST authorizer refuses every request";
    let (router, engine) = emulator();
    // The route's own backend, which this case proves is never reached, and the authorizer's
    // function, which refuses.
    let backend = given_function(&router, &engine, case, "unreached-backend", ECHO_EVENT).await;
    let gate = given_function(&router, &engine, case, "refusing-gate", DENIES).await;

    let (status, api) = control(
        &router,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "gated-metrics-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = api
        .get("apiId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let host = api
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_default();

    let (status, integration) = control(
        &router,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(lambda_integration(&backend, "2.0")),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration answered {status} {integration}"
    );
    let integration_id = integration
        .get("integrationId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let (status, authorizer) = control(
        &router,
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(json!({
            "name": "refusing-gate",
            "authorizerType": "REQUEST",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{gate}/invocations"
            ),
            "authorizerPayloadFormatVersion": "2.0",
            "enableSimpleResponses": true,
            "authorizerResultTtlInSeconds": 0,
            "identitySource": ["$request.header.authorization"]
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateAuthorizer answered {status} {authorizer}"
    );
    let authorizer_id = authorizer
        .get("authorizerId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let (status, route) = control(
        &router,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": GATED_ROUTE_KEY,
            "target": format!("integrations/{integration_id}"),
            "authorizationType": "CUSTOM",
            "authorizerId": authorizer_id
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute answered {status} {route}"
    );
    let (status, stage) = control(
        &router,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": GATED_STAGE, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage answered {status} {stage}"
    );

    let path = format!("/{GATED_STAGE}{GATED_ROUTE_PATH}");
    for attempt in 0..REFUSED_REQUESTS {
        let answered = send(
            &router,
            &host,
            "GET",
            &path,
            &[("authorization", "Bearer opaque-request-token")],
        )
        .await;
        assert_eq!(
            answered.status, FORBIDDEN,
            "case {case:?}: expected request {attempt} at {path:?} to be refused \
             {FORBIDDEN}, got {}",
            answered.summary()
        );
    }

    let refused = REFUSED_REQUESTS as f64;
    for (scope, dimensions) in [
        ("the api's own series", vec![(API_ID, api_id.clone())]),
        (
            "the stage's series",
            vec![(API_ID, api_id.clone()), (STAGE, GATED_STAGE.to_string())],
        ),
    ] {
        for (metric, expected) in [
            (COUNT, refused),
            (CLIENT_ERRORS, refused),
            (SERVER_ERRORS, 0.0),
        ] {
            let actual = total(&router, metric, &dimensions).await;
            assert_eq!(
                actual,
                Some(expected),
                "case {case:?}: expected {scope} to hold {expected} in {metric}, got {actual:?}"
            );
        }
        let actual = samples(&router, INTEGRATION_LATENCY, &dimensions).await;
        assert_eq!(
            actual,
            Some(0.0),
            "case {case:?}: expected {scope} to hold no {INTEGRATION_LATENCY} sample at all, \
             got {actual:?}"
        );
    }
    // And no backend-latency series exists for the api at all: nothing was relayed to the
    // route's integration, so there is nothing that series could be about.
    let held = series(&router, &api_id).await;
    assert!(
        !held
            .iter()
            .any(|(metric, _)| metric == INTEGRATION_LATENCY),
        "case {case:?}: expected no {INTEGRATION_LATENCY} series for an api whose backend was \
         never reached, got {held:?}"
    );
}

/// The dimension pairs of a scope, ordered by name the way a series reports them.
pub(crate) fn pairs(dimensions: &[(&str, String)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = dimensions
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect();
    pairs.sort();
    pairs
}
