// SPDX-License-Identifier: CC0-1.0

//! What a request made at the address an HTTP api PUBLISHES leaves behind in CloudWatch: the
//! series the stage that served it raises.
//!
//! RULE: **an api's `ApiEndpoint` is an address THIS emulator serves the api's data plane at,
//! so the invoke URL a client builds from the published value — that value with the stage name
//! appended ("the stage name is typically appended to this URI to form a complete path to a
//! deployed API stage", `Api$ApiEndpoint`), the reserved stage that serves the api's root being
//! reached with no stage segment at all — is resolved to the api's route and ACCOUNTED FOR by
//! the stage that served it: one `Count` observation per request in API Gateway's own
//! `AWS/ApiGateway` namespace, and one observation of the error counter of the answer's class
//! (`4xx` for a client error, `5xx` for a server error), under the dimension sets `{ApiId}` and
//! `{ApiId, Stage}` — and under the route's own dimensions as well where the serving stage asks
//! for detailed metrics. Whichever endpoint the client that MINTED the api was configured with,
//! the address that client was given is one its requests reach, so no traffic of a deployed
//! stage goes uncounted.**
//!
//! Three crates have to agree for that to hold — API Gateway v2 publishes the address and
//! resolves a request on it to the route, AWS Lambda answers about the function an `AWS_PROXY`
//! route names, and CloudWatch holds the series the serving stage publishes into — so the suite
//! lives here rather than in any one crate.
//!
//! Every request below is addressed at the value the api PUBLISHED (never at a path written out
//! here), and every count is read back through CloudWatch's own readers (`ListMetrics` and
//! `GetMetricStatistics`), so neither a published address nothing serves nor an observation
//! that did not reach that store can pass.
//!
//! The rows differ along what the rule generalises over: the endpoint the control-plane client
//! was configured with (an IPv4 loopback literal, a loopback NAME, a bracketed IPv6 literal,
//! and one of AWS's own service hosts — which publish the two different forms of invoke
//! address), the stage the api is deployed to (the reserved stage that serves the api's root and
//! named ones), the `RouteKey`'s form (a verb with a path, and the default route that names
//! neither half), the STATUS CLASS the stage answers the route in (the `307` that sends a caller
//! to an `HTTP_PROXY` upstream, which is in neither error class; the `500` of an integration
//! that produced no response; and the `429` a route throttle admitting nothing produces before
//! the backend is reached), how many requests are made, and whether the stage asks for the
//! route's own series.
//!
//! What the owning crates' own suites pin and this one does not restate: which observations ONE
//! answered request contributes to which series, and which requests are counted in none of them
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/`
//! `http_api_request_metrics.rs`); the FORM of the published address (that crate's
//! `http_api_published_invoke_address.rs`, and
//! [`super::apigatewayv2_published_endpoint_runs_its_lambda_route`], whose `announced_parts` is
//! the one reading of that value this suite shares); the wording of each refusal (that crate's
//! `http_api_backend_dispatch.rs` and `http_api_lambda_invoke_permission.rs`); and what the
//! datapoints then DECIDE — the alarm over these series and the delivery of its notification
//! (that crate's `http_api_stage_metrics.rs`, and the CloudWatch crate's own alarm-action
//! suites).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigatewayv2_published_endpoint_runs_its_lambda_route::announced_parts;

/// The Region every request here is signed for, and so the Region the apis of these rows live
/// in.
const REGION: &str = "us-east-1";

/// The namespace API Gateway publishes what its apis answered in.
const NAMESPACE: &str = "AWS/ApiGateway";

/// The request counter and the two error counters of an HTTP api, spelled out here rather than
/// imported, so a wrong constant in the implementation cannot make this suite pass.
const COUNT: &str = "Count";
const CLIENT_ERRORS: &str = "4xx";
const SERVER_ERRORS: &str = "5xx";

/// The dimensions those series are identified by: the two stage-level ones, and the two a
/// route's own series adds — the verb and the path halves of its `RouteKey`.
const API_ID: &str = "ApiId";
const STAGE: &str = "Stage";
const METHOD: &str = "Method";
const RESOURCE: &str = "Resource";

/// The reserved stage "that serves an api's root" (`$default`), reached with no stage segment in
/// the path at all, and the default ROUTE, which answers whatever no other route of the api
/// matches and names neither half of a route key.
const ROOT_STAGE: &str = "$default";
const DEFAULT_ROUTE: &str = "$default";

/// The verb a route key that names no verb answers, as API Gateway's own key grammar spells
/// every verb.
const ANY_METHOD: &str = "ANY";

/// The verb every request below is made with: the one the rows' explicit route keys answer, and
/// one of the many the default route answers.
const VERB: &str = "GET";

/// The upstream the `HTTP_PROXY` rows front, and the status such a route answers with: the
/// upstream is a URI the CALLER reaches, so the gateway sends the caller there rather than
/// forwarding anything itself — nothing leaves this test, and the redirect carrying that URI is
/// what proves the request was resolved all the way to the route's integration.
const UPSTREAM: &str = "https://upstream.example.com/items";
const SENT_TO_UPSTREAM: u16 = 307;

/// The status an integration that produced no response is answered with, and the status a
/// request the serving stage's route settings throttle is refused with.
const INTEGRATION_FAULT: u16 = 500;
const THROTTLED: u16 = 429;

/// The front door's error code for a request NO service claimed. The defect this suite pins is a
/// published address nothing serves, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The window every metric read covers, in seconds, and the period it is read at: generous on
/// both sides, so no row depends on where in a minute the suite runs.
const WINDOW_SECONDS: f64 = 900.0;
const AHEAD_SECONDS: f64 = 120.0;
const PERIOD_SECONDS: i64 = 60;

/// The function an `AWS_PROXY` row's integration names: a well-formed function ARN of this
/// account that AWS Lambda holds NO function for, so that service answers about it
/// (`ResourceNotFoundException` for its resource policy, which grants nothing) and the gateway
/// answers its own [`INTEGRATION_FAULT`] for a route whose integration produced no response —
/// the server-error arm, reached without any code running.
fn absent_function_arn() -> String {
    format!(
        "arn:aws:lambda:{REGION}:{}:function:apigwv2-no-such-function",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// API Gateway v2, AWS Lambda and CloudWatch over ONE engine, as `core/router/src/lib.rs` mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published address
    // carries none, as a client of a published HTTP api endpoint sends none.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`], which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One API Gateway v2 control-plane call, addressed at `host` — the endpoint the CLIENT reached,
/// which is what decides the address a minted api announces — as `(status, parsed body)`.
async fn control(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One CloudWatch call over its `awsJson1_0` wire, as `(status, parsed body)`.
async fn monitoring(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header("authorization", authorization("monitoring"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// What a dialled address answered: its status, the front door's error code, where it sends the
/// caller, and the body verbatim.
struct Answered {
    status: u16,
    error_type: Option<String>,
    location: Option<String>,
    body: Bytes,
}

impl Answered {
    /// Enough of the answer to identify it in an assertion message.
    fn summary(&self) -> String {
        format!(
            "{} {:?} (location: {:?}, x-amzn-errortype: {:?})",
            self.status,
            String::from_utf8_lossy(&self.body),
            self.location,
            self.error_type
        )
    }
}

/// Send one request to `authority` at `path`, as a client of a published HTTP api endpoint makes
/// it: unsigned, carrying no credential of any kind.
///
/// The request is handed straight to the router IN PROCESS, so no socket is opened and nothing
/// leaves this test: the authority is only the address the request carries.
async fn invoke(router: &MultiServiceRouter, authority: &str, path: &str) -> Answered {
    let request = http::Request::builder()
        .method(VERB)
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
    };
    let error_type = header("x-amzn-errortype");
    let location = header("location");
    Answered {
        status: response.status().as_u16(),
        error_type,
        location,
        body: response.into_body(),
    }
}

/// The member `member` of `body`, as text.
fn as_str(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {body}"))
        .to_string()
}

/// Now, as the epoch seconds a CloudWatch read states its window in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// The `Sum` of every datapoint the series `metric`/`dimensions` holds in the window, and `0.0`
/// for a series holding none at all.
///
/// Summed over the datapoints rather than read off one, because a period boundary crossed while
/// a row is running splits its observations over two windows — and the whole of what a counter
/// says is its total.
async fn total(router: &MultiServiceRouter, metric: &str, dimensions: &[(&str, String)]) -> f64 {
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
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetMetricStatistics for {metric} {dimensions:?} answered {status} {body}"
    );
    body.get("Datapoints")
        .and_then(Value::as_array)
        .map(|points| {
            points
                .iter()
                .map(|point| point.get("Sum").and_then(Value::as_f64).unwrap_or_default())
                .sum()
        })
        .unwrap_or_default()
}

/// Every series this namespace holds for `api_id`, as `(metric name, its dimension pairs)` with
/// the pairs ordered by dimension name.
async fn series(router: &MultiServiceRouter, api_id: &str) -> Vec<(String, Vec<(String, String)>)> {
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
                        .map(|dimensions| {
                            dimensions
                                .iter()
                                .map(|dimension| {
                                    (
                                        as_str("the listed series", dimension, "Name"),
                                        as_str("the listed series", dimension, "Value"),
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

/// The backend a row's route fronts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// An `HTTP_PROXY` upstream the CALLER reaches, so the route answers the redirect that sends
    /// it there.
    Upstream,
    /// An `AWS_PROXY` integration naming a function AWS Lambda holds none of, so the route
    /// answers the gateway's own status for an integration that produced no response.
    AbsentFunction,
}

impl Backend {
    /// The `CreateIntegration` body this backend is declared with.
    fn integration(self) -> Value {
        match self {
            Self::Upstream => json!({
                "integrationType": "HTTP_PROXY",
                "integrationMethod": VERB,
                "integrationUri": UPSTREAM,
                "payloadFormatVersion": "1.0",
            }),
            Self::AbsentFunction => json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": absent_function_arn(),
                "payloadFormatVersion": "2.0",
            }),
        }
    }
}

/// One row: the endpoint the control-plane client was configured with, the stage the api is
/// deployed to, the route it serves and how that route is answered.
struct Row {
    case: &'static str,
    /// The `Host` every control-plane call of this row carries — the endpoint the client is
    /// configured with, which is what decides the address the minted api announces.
    host: &'static str,
    stage_name: &'static str,
    /// The route the api holds, and the path within the stage each request addresses.
    route_key: &'static str,
    route_path: &'static str,
    backend: Backend,
    /// How many requests are made at the published address.
    requests: u32,
    /// The status every one of them is answered with.
    expected_status: u16,
    /// The `(rate, burst)` the stage states for the route, or none at all. A pair admitting
    /// nothing is the throttle that refuses every request before the backend is reached.
    throttle: Option<(f64, i32)>,
    /// Whether the stage asks for the route's own series
    /// (`RouteSettings$DetailedMetricsEnabled`, stated on the stage's defaults).
    detailed: bool,
}

/// The rows: the three forms of this emulator's own endpoint a client on the machine it runs on
/// is configured with and one of AWS's own service hosts, each with its own stage form, route-key
/// form, backend, status class, request count and detailed-metrics opt-in.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: a client configured with the emulator's loopback \
                   endpoint, the stage that serves the api's root, a route sent to its upstream",
            host: "127.0.0.1:45923",
            stage_name: ROOT_STAGE,
            route_key: "GET /items",
            route_path: "/items",
            backend: Backend::Upstream,
            requests: 2,
            expected_status: SENT_TO_UPSTREAM,
            throttle: None,
            detailed: false,
        },
        Row {
            case: "the same endpoint named by its loopback NAME, a named stage whose route's \
                   integration produces no response, with the route's own series asked for",
            host: "localhost:9297",
            stage_name: "prod",
            route_key: "GET /boom",
            route_path: "/boom",
            backend: Backend::AbsentFunction,
            requests: 3,
            expected_status: INTEGRATION_FAULT,
            throttle: None,
            detailed: true,
        },
        Row {
            case: "the same endpoint named by a bracketed IPv6 literal, a named stage whose \
                   route throttle admits nothing",
            host: "[::1]:9297",
            stage_name: "live",
            route_key: "GET /items",
            route_path: "/items",
            backend: Backend::Upstream,
            requests: 2,
            expected_status: THROTTLED,
            throttle: Some((0.0, 0)),
            detailed: false,
        },
        Row {
            case: "a client that resolves AWS's own hosts, the stage that serves the api's \
                   root, a throttled route whose own series the stage asks for",
            host: "apigateway.us-east-1.amazonaws.com",
            stage_name: ROOT_STAGE,
            route_key: "GET /items",
            route_path: "/items",
            backend: Backend::Upstream,
            requests: 1,
            expected_status: THROTTLED,
            throttle: Some((0.0, 0)),
            detailed: true,
        },
        Row {
            case: "a client that resolves AWS's own hosts, a named stage whose DEFAULT route \
                   answers a path no other route matches, with its own series asked for",
            host: "apigateway.us-east-1.amazonaws.com",
            stage_name: "staging",
            route_key: DEFAULT_ROUTE,
            route_path: "/anything/at/all",
            backend: Backend::Upstream,
            requests: 2,
            expected_status: SENT_TO_UPSTREAM,
            throttle: None,
            detailed: true,
        },
    ]
}

/// The stage this row creates: its name, auto-deploy, the route settings its throttle is stated
/// in and the defaults its detailed-metrics opt-in is stated in.
fn stage_of(row: &Row) -> Value {
    let mut stage = json!({ "stageName": row.stage_name, "autoDeploy": true });
    if let Some((rate, burst)) = row.throttle {
        stage["routeSettings"] = json!({
            row.route_key: {
                "throttlingRateLimit": rate,
                "throttlingBurstLimit": burst,
            },
        });
    }
    if row.detailed {
        stage["defaultRouteSettings"] = json!({ "detailedMetricsEnabled": true });
    }
    stage
}

/// Deploy the row's api THROUGH ITS OWN control-plane endpoint: the row's integration, the route
/// that fronts it and an auto-deploying stage. Answers
/// `(ApiId, the ApiEndpoint CreateApi published)`.
async fn given_api(router: &MultiServiceRouter, row: &Row, name: &str) -> (String, String) {
    let case = row.case;
    let (status, api) = control(
        router,
        row.host,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {status} {api}"
    );
    let api_id = as_str(case, &api, "apiId");
    let announced = as_str(case, &api, "apiEndpoint");

    let (status, integration) = control(
        router,
        row.host,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(row.backend.integration()),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {status} {integration}"
    );
    let integration_id = as_str(case, &integration, "integrationId");

    let (status, route) = control(
        router,
        row.host,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": row.route_key,
            "target": format!("integrations/{integration_id}"),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {status} {route}"
    );

    let (status, stage) = control(
        router,
        row.host,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(stage_of(row)),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {status} {stage}"
    );
    assert!(
        stage.get("deploymentId").and_then(Value::as_str).is_some(),
        "case {case:?}: an auto-deploying stage must carry a deployment, got {stage}"
    );

    (api_id, announced)
}

/// The path a client builds from the published endpoint's own base path: the stage name ahead of
/// the route's path, and nothing ahead of it for the stage that serves the api's root.
fn invoke_path(row: &Row, base_path: &str) -> String {
    if row.stage_name == ROOT_STAGE {
        return format!("{base_path}{}", row.route_path);
    }
    format!("{base_path}/{}{}", row.stage_name, row.route_path)
}

/// The verb and path halves of a `RouteKey`, which is the verb and the path a route answers for
/// written as one value — the two dimensions a route's own series adds.
///
/// The default route names neither half: it answers whatever no other route of the api matches,
/// every verb included, so the key itself stands for the resource and the verb is the one API
/// Gateway's key grammar spells as every verb ([`ANY_METHOD`]). Both forms are rows above.
fn route_dimensions(route_key: &str) -> (String, String) {
    match route_key.split_once(' ') {
        Some((method, path)) => (method.trim().to_string(), path.trim().to_string()),
        None => (ANY_METHOD.to_string(), route_key.to_string()),
    }
}

/// How many of `requests` land in a counter whose class the answers belong to: one observation
/// per request of that class, and nothing for answers outside it.
fn counted(requests: u32, class_holds: bool) -> f64 {
    if class_holds {
        f64::from(requests)
    } else {
        0.0
    }
}

/// RULE: every request a client makes at the address the api PUBLISHED reaches the api's route
/// and is counted by the stage that served it — in `Count`, in the error counter its status's
/// class names, under `{ApiId}` and `{ApiId, Stage}`, and under the route's own dimensions
/// exactly where the stage asked for them.
#[tokio::test]
async fn traffic_at_the_published_api_endpoint_raises_the_serving_stages_own_series() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (api_id, announced) =
            given_api(&router, &row, &format!("metered-endpoint-{index}")).await;

        // The value is settled by the operation that MINTED the api, so the client reads back
        // the same address it was first given.
        let (status, fetched) =
            control(&router, row.host, "GET", &format!("/v2/apis/{api_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetApi must answer 200, got {status} {fetched}"
        );
        assert_eq!(
            as_str(case, &fetched, "apiEndpoint"),
            announced,
            "case {case:?}: GetApi must report the published ApiEndpoint {announced:?}, got \
             {fetched}"
        );

        let (authority, base_path) = announced_parts(&announced);
        let path = invoke_path(&row, &base_path);
        let api_dimensions = [(API_ID, api_id.clone())];
        let stage_dimensions = [(API_ID, api_id.clone()), (STAGE, row.stage_name.to_string())];

        // Before any traffic this api's stage has answered nothing, so what the reads below
        // find is this row's own requests and not a count that was already there.
        let before = total(&router, COUNT, &api_dimensions).await;
        assert_eq!(
            before, 0.0,
            "case {case:?}: the {COUNT} series of an api whose stage has answered nothing must \
             hold nothing, got {before}"
        );

        for attempt in 0..row.requests {
            let answered = invoke(&router, &authority, &path).await;
            assert_ne!(
                answered.error_type.as_deref(),
                Some(UNKNOWN_SERVICE),
                "case {case:?}: the published address must be one this emulator serves, but \
                 request {attempt} at {authority}{path} is claimed by no service: {}",
                answered.summary()
            );
            assert_eq!(
                answered.status,
                row.expected_status,
                "case {case:?}: request {attempt} at the published address {authority}{path} \
                 must be answered {}, got {}",
                row.expected_status,
                answered.summary()
            );
            // A request the route's upstream answered carries that upstream, which is what
            // proves it was resolved all the way to the route's own integration.
            let expected_location = (row.expected_status == SENT_TO_UPSTREAM).then_some(UPSTREAM);
            assert_eq!(
                answered.location.as_deref(),
                expected_location,
                "case {case:?}: request {attempt} must carry {expected_location:?} as where it \
                 is sent, got {}",
                answered.summary()
            );
        }

        // What the stage published about them: the request counter, and the error counter of the
        // class its answers fall in.
        let requests = f64::from(row.requests);
        let expected = [
            (COUNT, requests),
            (
                CLIENT_ERRORS,
                counted(row.requests, (400..500).contains(&row.expected_status)),
            ),
            (
                SERVER_ERRORS,
                counted(row.requests, (500..600).contains(&row.expected_status)),
            ),
        ];
        for (named, dimensions) in [
            ("{ApiId}", api_dimensions.as_slice()),
            ("{ApiId, Stage}", stage_dimensions.as_slice()),
        ] {
            for (metric, wanted) in expected {
                let actual = total(&router, metric, dimensions).await;
                assert_eq!(
                    actual, wanted,
                    "case {case:?}: the {metric} series under {named} must total {wanted} for \
                     the {requests} requests the published address answered {}, got {actual}",
                    row.expected_status
                );
            }
        }

        // And which series the namespace then holds for this api: the two stage-level sets
        // always, the route's own set exactly where the stage asked for it.
        let held = series(&router, &api_id).await;
        for wanted in [
            vec![(API_ID.to_string(), api_id.clone())],
            vec![
                (API_ID.to_string(), api_id.clone()),
                (STAGE.to_string(), row.stage_name.to_string()),
            ],
        ] {
            assert!(
                held.iter()
                    .any(|(metric, dimensions)| metric == COUNT && *dimensions == wanted),
                "case {case:?}: the stage's {COUNT} series must be published under {wanted:?}, \
                 got {held:?}"
            );
        }

        let (method, resource) = route_dimensions(row.route_key);
        if row.detailed {
            let route_dimensions = [
                (API_ID, api_id.clone()),
                (STAGE, row.stage_name.to_string()),
                (METHOD, method.clone()),
                (RESOURCE, resource.clone()),
            ];
            for (metric, wanted) in expected {
                let actual = total(&router, metric, &route_dimensions).await;
                assert_eq!(
                    actual, wanted,
                    "case {case:?}: the {metric} series of the route the stage asked for \
                     detailed metrics on ({method} {resource}) must total {wanted}, got {actual} \
                     ({held:?})"
                );
            }
        } else {
            assert!(
                !held
                    .iter()
                    .any(|(_, dimensions)| dimensions.iter().any(|(name, _)| name == METHOD)),
                "case {case:?}: a stage that asks for no detailed metrics must publish no \
                 route's own series, got {held:?}"
            );
        }
    }
}
