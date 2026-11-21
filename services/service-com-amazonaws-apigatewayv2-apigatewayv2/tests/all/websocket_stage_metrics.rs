// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ApiGateway` series a WEBSOCKET stage publishes about the traffic it carried, read
//! back through CloudWatch the way a dashboard or an alarm reads them.
//!
//! The rule every table below is one slice of: **a deployed WEBSOCKET stage publishes, in
//! namespace `AWS/ApiGateway` under the dimension set {ApiId, Stage}, what its own traffic
//! did — one `ConnectCount` per connection request sent to the `$connect` route's
//! INTEGRATION, one `MessageCount` per message
//! it carried in either direction (each call dispatched to a route, the connection request and
//! the close included, and each frame written back to the client, a route's reply, the
//! gateway's own frame and an out-of-band `PostToConnection` push alike), one
//! `IntegrationLatency` sample per integration that answered, and `ClientError`,
//! `ExecutionError` and `IntegrationError` once per CALL — carrying 1 for the ONE class that
//! call earned and an explicit 0 for the two classes it did not — and publishes the same
//! counters additionally under {ApiId, Stage, Route} for a route whose stage turns
//! `DetailedMetricsEnabled` on — in that route's own `RouteSettings` where the stage names
//! them, and in the `DefaultRouteSettings` every route it does not name is served under
//! otherwise — while a route the setting is off for is counted in the stage-wide series
//! alone.**
//!
//! The rows span what the rule generalises over: the stage (several stages of ONE api, each
//! carrying its own traffic), the kind of call (the
//! connection request, a two-way route, a route whose function raises, a message the api
//! routes nowhere, the close), the direction of the message (a frame in, a reply out, a
//! gateway frame out, a push out), the outcome (an integration that answered, one that
//! produced no reply, a connection request refused with a client-error status, an api that
//! fronts no `$connect` integration at all), where `DetailedMetricsEnabled` is stated (nowhere,
//! the stage default, one route alone, and one route contradicting the default in either
//! direction) and the dimension set each measurement lands under.
//!
//! Nothing here is a value this emulator fills in: the traffic is driven over a real socket
//! against functions AWS Lambda RAN, and every assertion is on what CloudWatch answers for the
//! series — which is why Lambda, the `@connections` management API and CloudWatch are all
//! mounted beside this service on one engine.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::given_function;
use crate::websocket_transport_data_plane::{
    exchange, exchanged_reply, handshake_at, ClientSocket,
};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The namespace an API Gateway stage publishes its own metrics to.
const NAMESPACE: &str = "AWS/ApiGateway";

/// The dimensions those series are published under.
const API_ID_DIMENSION: &str = "ApiId";
const STAGE_DIMENSION: &str = "Stage";
const ROUTE_DIMENSION: &str = "Route";

/// How long a case waits for the series to reach the values the traffic owes them, and how
/// often it looks again: the last call of a connection (`$disconnect`) is served after the
/// client has closed, so what is waited for is a datapoint the traffic ALREADY owes rather
/// than anything a clock schedules. Generous, so a loaded machine does not fail a case.
const SERIES_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The window every read below spans, before and after the instant it is made at: wide enough
/// to hold the whole of a case's traffic whichever minute bucket each datapoint landed in.
const WINDOW: f64 = 600.0;

/// The period a read lays its windows at: one minute, the resolution a regular-resolution
/// datapoint is stored at.
const PERIOD: i32 = 60;

/// This service, AWS Lambda, the `@connections` management API and CloudWatch on ONE engine,
/// with request authentication off.
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
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One control-plane request against THIS service, as its status and parsed body.
async fn control(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("the body serializes")),
        None => bytes::Bytes::new(),
    };
    let request = smithy_server_runtime::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(payload)
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One control-plane request that must answer `201`.
async fn created(
    emulator: &MultiServiceRouter,
    case: &str,
    what: &str,
    uri: &str,
    body: Value,
) -> Value {
    let (status, answered) = control(emulator, "POST", uri, Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {what} must answer 201, got {status} {answered}"
    );
    answered
}

/// The text member `name` of `value`.
fn text(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A handler that answers `200` with the event it was given as its body, which is how a
/// two-way route's reply and the connection id it carries are read here.
pub(crate) const ECHO_HANDLER: &str = "import json\n\
     \n\
     \n\
     def lambda_handler(event, context):\n\
     \x20   return {\"statusCode\": 200, \"body\": json.dumps(event)}\n";

/// A handler that answers `200` and nothing else: the lifecycle routes, whose answer reaches
/// no client.
pub(crate) const OK_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   return {\"statusCode\": 200}\n";

/// A handler that RAISES, so its route's integration produces no reply at all.
const RAISING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   raise RuntimeError(\"this route fails\")\n";

/// A `$connect` handler that REFUSES the connection with a client-error status, which is what
/// a backend that does not admit a client answers.
const REFUSING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   return {\"statusCode\": 403, \"body\": \"no\"}\n";

/// One api, created with the socket its `ApiEndpoint` announces.
pub(crate) struct Api {
    pub(crate) api_id: String,
    /// The `host:port` of the api's own socket, read off the announced `ApiEndpoint` so a case
    /// dials the address the service reported.
    pub(crate) authority: String,
}

/// Create a WEBSOCKET api whose frames select a route by their `action` member.
pub(crate) async fn given_api(emulator: &MultiServiceRouter, case: &str, name: &str) -> Api {
    let created = created(
        emulator,
        case,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
    )
    .await;
    let announced = text(&created, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
        })
        .to_string();
    Api {
        api_id: text(&created, "apiId"),
        authority,
    }
}

/// Bind `function_arn` as an `AWS_PROXY` integration of `api_id` and route `route_key` to it,
/// with a `RouteResponse` when `two_way` — "to define a two-way route, create a route
/// response".
pub(crate) async fn given_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    function_arn: &str,
    two_way: bool,
) {
    let integration = created(
        emulator,
        case,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_arn,
            "integrationMethod": "POST"
        }),
    )
    .await;
    let integration_id = text(&integration, "integrationId");
    let route = created(
        emulator,
        case,
        "CreateRoute",
        &format!("/v2/apis/{api_id}/routes"),
        json!({
            "routeKey": route_key,
            "target": format!("integrations/{integration_id}")
        }),
    )
    .await;
    if two_way {
        let route_id = text(&route, "routeId");
        created(
            emulator,
            case,
            "CreateRouteResponse",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            json!({ "routeResponseKey": "$default" }),
        )
        .await;
    }
}

/// Create one stage of `api_id` and release a deployment to it, with `DetailedMetricsEnabled`
/// in its `DefaultRouteSettings` when `detailed`.
pub(crate) async fn given_stage(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
    detailed: bool,
) {
    given_configured_stage(emulator, case, api_id, stage, detailed.then_some(true), &[]).await;
}

/// Create one stage of `api_id` and release a deployment to it, stating
/// `DetailedMetricsEnabled` where the caller places it: in the stage's `DefaultRouteSettings`
/// (`default_detailed`, left out of the request entirely when `None`) and in the
/// `RouteSettings` of each route `route_detailed` names.
///
/// The ONE create-and-deploy both forms go through, so every stage a case of this suite drives
/// is provisioned the same way.
async fn given_configured_stage(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
    default_detailed: Option<bool>,
    route_detailed: &[(&str, bool)],
) {
    let mut body = json!({ "stageName": stage });
    if let Some(enabled) = default_detailed {
        body["defaultRouteSettings"] = json!({ "detailedMetricsEnabled": enabled });
    }
    if !route_detailed.is_empty() {
        let settings: serde_json::Map<String, Value> = route_detailed
            .iter()
            .map(|(route_key, enabled)| {
                (
                    (*route_key).to_string(),
                    json!({ "detailedMetricsEnabled": enabled }),
                )
            })
            .collect();
        body["routeSettings"] = Value::Object(settings);
    }
    created(
        emulator,
        case,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        body,
    )
    .await;
    created(
        emulator,
        case,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": stage }),
    )
    .await;
}

/// Push `payload` to `connection_id` through the `@connections` management API of `stage`,
/// which must answer `200`: an out-of-band message the api sends the client.
///
/// The STAGE is part of the endpoint — these operations are published at
/// `…/{stage}/@connections/{id}` — so an addressing that names no deployed stage serves
/// nothing at all and the push would be refused `403` instead.
pub(crate) async fn pushed(
    emulator: &MultiServiceRouter,
    case: &str,
    stage: &str,
    connection_id: &str,
    payload: &str,
) {
    let host = format!("execute-api.{REGION}.amazonaws.com");
    let addressed = crate::path_label::addressed(connection_id);
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/{stage}/@connections/{addressed}"))
        .header("host", &host)
        .header("authorization", authorization("execute-api"))
        .body(bytes::Bytes::from(payload.to_string()))
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status, 200,
        "case {case:?}: PostToConnection must answer 200, got {status}"
    );
}

/// One expected series of a stage: the metric, the route dimension it is published under (or
/// none for the stage-wide series), the statistic read and the value the traffic owes it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Expected {
    metric: &'static str,
    /// The `Route` dimension for a per-route series; `None` for the stage-wide one.
    route: Option<&'static str>,
    /// The statistic read: `Sum` for a counter, and `SampleCount` for the latency series,
    /// whose own values are measured durations rather than something the traffic fixes.
    statistic: &'static str,
    value: f64,
}

/// A stage-wide counter, read by its `Sum`.
pub(crate) fn counter(metric: &'static str, value: f64) -> Expected {
    Expected {
        metric,
        route: None,
        statistic: "Sum",
        value,
    }
}

/// A stage-wide series read by its `SampleCount`: how many OBSERVATIONS it holds, which is
/// what tells an error counter standing at an explicit zero from one no call ever published.
pub(crate) fn samples(metric: &'static str, value: f64) -> Expected {
    Expected {
        metric,
        route: None,
        statistic: "SampleCount",
        value,
    }
}

/// The stage-wide latency series, read by its `SampleCount`: one sample per integration that
/// answered.
pub(crate) fn latency_samples(value: f64) -> Expected {
    Expected {
        metric: "IntegrationLatency",
        route: None,
        statistic: "SampleCount",
        value,
    }
}

/// The per-route series of `route`, read the same way its stage-wide counterpart is.
pub(crate) fn per_route(metric: &'static str, route: &'static str, value: f64) -> Expected {
    Expected {
        metric,
        route: Some(route),
        statistic: if metric == "IntegrationLatency" {
            "SampleCount"
        } else {
            "Sum"
        },
        value,
    }
}

/// What CloudWatch reports for ONE series over the window this read spans: the statistic
/// `expected` names, folded over every datapoint in it (a series with no datapoint at all
/// reports nothing, which is zero of whatever was asked for).
async fn observed(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
    expected: &Expected,
) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is past the epoch")
        .as_secs_f64();
    let mut dimensions = vec![
        json!({ "Name": API_ID_DIMENSION, "Value": api_id }),
        json!({ "Name": STAGE_DIMENSION, "Value": stage }),
    ];
    if let Some(route) = expected.route {
        dimensions.push(json!({ "Name": ROUTE_DIMENSION, "Value": route }));
    }
    let host = format!("monitoring.{REGION}.amazonaws.com");
    let body = json!({
        "Namespace": NAMESPACE,
        "MetricName": expected.metric,
        "Dimensions": dimensions,
        "StartTime": now - WINDOW,
        "EndTime": now + WINDOW,
        "Period": PERIOD,
        "Statistics": ["Sum", "SampleCount"],
    });
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            "GraniteServiceVersion20100801.GetMetricStatistics",
        )
        .header("authorization", authorization("monitoring"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let answered: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics for {metric:?} must answer 200, got {status} \
         {answered}",
        metric = expected.metric
    );
    answered
        .get("Datapoints")
        .and_then(Value::as_array)
        .map(|datapoints| {
            datapoints
                .iter()
                .filter_map(|datapoint| datapoint.get(expected.statistic))
                .filter_map(Value::as_f64)
                .sum()
        })
        .unwrap_or(0.0)
}

/// Wait until every expectation of `stage` holds, and fail naming the case, the series and
/// what was actually observed when the deadline passes.
///
/// A counted datapoint is owed by a call that has already been made, so this looks again for
/// the condition rather than waiting a fixed time for it.
pub(crate) async fn reached(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    stage: &str,
    expected: &[Expected],
) {
    let deadline = Instant::now() + SERIES_DEADLINE;
    loop {
        let mut seen: Vec<String> = Vec::new();
        let mut all = true;
        for want in expected {
            let got = observed(emulator, case, api_id, stage, want).await;
            // Counts and sample counts are whole numbers, so a half unit apart is a
            // different answer; nothing here compares two measured durations.
            if (got - want.value).abs() > 0.5 {
                all = false;
            }
            seen.push(format!(
                "{metric} route={route:?} {statistic}={got} (expected {want_value})",
                metric = want.metric,
                route = want.route,
                statistic = want.statistic,
                want_value = want.value,
            ));
        }
        if all {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: stage {stage:?} of api {api_id:?} must publish the series its \
             traffic produced; got {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Close the client's end of a connection, which is what makes the `$disconnect` call due.
pub(crate) async fn closed(case: &str, socket: ClientSocket) {
    let mut socket = socket;
    futures_util::SinkExt::close(&mut socket)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the client must close, got {error}"));
    drop(socket);
}

/// Two stages of ONE api, each carrying its own traffic: the counters each publishes are its
/// own, and only the stage with `DetailedMetricsEnabled` publishes per-route series.
#[tokio::test]
async fn each_stage_publishes_the_counters_its_own_traffic_produced() {
    let case = "two stages of one WebSocket api, one of them with detailed metrics";
    let (emulator, engine) = emulator();

    // One function per behaviour a route can have, as a client provisions them.
    let lifecycle = given_function(&emulator, &engine, case, "ws-lifecycle", OK_HANDLER).await;
    let echo = given_function(&emulator, &engine, case, "ws-echo", ECHO_HANDLER).await;
    let raising = given_function(&emulator, &engine, case, "ws-raising", RAISING_HANDLER).await;

    let api = given_api(&emulator, case, "stage-metrics-api").await;
    for (route_key, function, two_way) in [
        ("$connect", &lifecycle, false),
        ("$disconnect", &lifecycle, false),
        ("$default", &lifecycle, false),
        ("reply", &echo, true),
        ("boom", &raising, false),
    ] {
        given_route(&emulator, case, &api.api_id, route_key, function, two_way).await;
    }
    given_stage(&emulator, case, &api.api_id, "basic", false).await;
    given_stage(&emulator, case, &api.api_id, "detailed", true).await;

    // The stage with no detailed metrics: a two-way frame answered by its route, and a frame
    // the `$default` route serves one-way.
    let mut basic: ClientSocket = handshake_at(case, &api.authority, "/basic")
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));
    let replied = exchanged_reply(case, &mut basic, r#"{"action":"reply","n":1}"#).await;
    assert!(
        replied.contains("MESSAGE"),
        "case {case:?}: the two-way route must answer its own event, got {replied:?}"
    );
    assert!(
        exchange(case, &mut basic, r#"{"action":"nothing-matches"}"#)
            .await
            .is_none(),
        "case {case:?}: a one-way route sends nothing back"
    );
    closed(case, basic).await;

    // The stage WITH detailed metrics: the same two-way route, a route whose function raises,
    // and an out-of-band push to the connection.
    let mut detailed: ClientSocket = handshake_at(case, &api.authority, "/detailed")
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));
    let replied = exchanged_reply(case, &mut detailed, r#"{"action":"reply","n":2}"#).await;
    let event: Value = serde_json::from_str(&replied)
        .unwrap_or_else(|error| panic!("case {case:?}: expected the event back, got {error}"));
    let connection_id = event
        .pointer("/requestContext/connectionId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the reply must carry the connection id"))
        .to_string();
    let failed = exchanged_reply(case, &mut detailed, r#"{"action":"boom"}"#).await;
    assert!(
        failed.contains("Internal server error"),
        "case {case:?}: a route that produced no reply is answered with the gateway's own \
         frame, got {failed:?}"
    );
    pushed(&emulator, case, "detailed", &connection_id, r#"{"pushed":true}"#).await;
    let delivered = tokio::time::timeout(
        SERIES_DEADLINE,
        futures_util::StreamExt::next(&mut detailed),
    )
    .await;
    assert!(
        matches!(&delivered, Ok(Some(Ok(_)))),
        "case {case:?}: the pushed payload must arrive over the socket, got {delivered:?}"
    );
    closed(case, detailed).await;

    // The stage-wide series of the stage with NO detailed metrics: one connection request,
    // every message it carried (the two calls, the reply written back, and the `$connect` and
    // `$disconnect` calls), one latency sample per integration that answered, and no error of
    // any class. Its per-route series do not exist, so they read as zero.
    reached(
        &emulator,
        case,
        &api.api_id,
        "basic",
        &[
            counter("ConnectCount", 1.0),
            counter("MessageCount", 5.0),
            latency_samples(4.0),
            counter("ClientError", 0.0),
            counter("ExecutionError", 0.0),
            counter("IntegrationError", 0.0),
            // Each error counter carries an observation per CALL the stage served (the
            // connection request, the two frames and the close), every one of them a zero:
            // what a client reads over healthy traffic is an explicit nought rather than a
            // gap.
            samples("ClientError", 4.0),
            samples("ExecutionError", 4.0),
            samples("IntegrationError", 4.0),
            per_route("ConnectCount", "$connect", 0.0),
            per_route("MessageCount", "reply", 0.0),
            per_route("IntegrationLatency", "$connect", 0.0),
        ],
    )
    .await;

    // The stage WITH detailed metrics: its own stage-wide totals — the push is a message of
    // the stage that belongs to no route — and the per-route series beside them.
    reached(
        &emulator,
        case,
        &api.api_id,
        "detailed",
        &[
            counter("ConnectCount", 1.0),
            counter("MessageCount", 7.0),
            // One sample per invocation the backend ANSWERED: the three routes that returned
            // and the one whose function raised, which answered the invocation with the error
            // document Lambda reports for it.
            latency_samples(4.0),
            counter("ClientError", 0.0),
            // The route whose function raised is counted ONCE, in the class that names whose
            // failure it was: the function RAN and ended in an unhandled error, so the
            // route's execution is what did not reach a successful end and the integration's
            // own series stays at nought.
            counter("ExecutionError", 1.0),
            counter("IntegrationError", 0.0),
            // The same four calls, so the one failure stands in a series of four
            // observations rather than alone.
            samples("ClientError", 4.0),
            samples("ExecutionError", 4.0),
            samples("IntegrationError", 4.0),
            per_route("ConnectCount", "$connect", 1.0),
            per_route("MessageCount", "$connect", 1.0),
            per_route("IntegrationLatency", "$connect", 1.0),
            per_route("MessageCount", "reply", 2.0),
            per_route("IntegrationLatency", "reply", 1.0),
            per_route("MessageCount", "boom", 2.0),
            per_route("IntegrationLatency", "boom", 1.0),
            per_route("ExecutionError", "boom", 1.0),
            // The per-route series split the same ONE class the stage-wide ones carry.
            per_route("ClientError", "boom", 0.0),
            per_route("IntegrationError", "boom", 0.0),
            per_route("MessageCount", "$disconnect", 1.0),
            per_route("IntegrationLatency", "$disconnect", 1.0),
        ],
    )
    .await;
}

/// The two refusals a stage meters as a CLIENT error: a message the api routes nowhere, and a
/// connection request the `$connect` integration answered a client-error status for.
#[tokio::test]
async fn a_refused_call_is_metered_as_the_class_it_was_refused_with() {
    let case = "a message no route serves, and a connection request a backend refused";
    let (emulator, engine) = emulator();

    let lifecycle = given_function(&emulator, &engine, case, "ws-admitting", OK_HANDLER).await;
    let refusing = given_function(&emulator, &engine, case, "ws-refusing", REFUSING_HANDLER).await;

    // An api with a `$connect` route and NOTHING else: every frame a client sends it is routed
    // nowhere.
    let unrouted = given_api(&emulator, case, "unrouted-api").await;
    given_route(&emulator, case, &unrouted.api_id, "$connect", &lifecycle, false).await;
    given_stage(&emulator, case, &unrouted.api_id, "bare", false).await;

    let mut socket: ClientSocket = handshake_at(case, &unrouted.authority, "/bare")
        .await
        .unwrap_or_else(|status| panic!("case {case:?}: the handshake must complete, got {status}"));
    let answered = exchanged_reply(case, &mut socket, r#"{"action":"nowhere"}"#).await;
    assert!(
        answered.contains("Forbidden"),
        "case {case:?}: a message the api routes nowhere is answered with the gateway's \
         Forbidden frame, got {answered:?}"
    );
    closed(case, socket).await;

    reached(
        &emulator,
        case,
        &unrouted.api_id,
        "bare",
        &[
            counter("ConnectCount", 1.0),
            // The connection request, the frame the client sent and the gateway's own frame
            // back; the api holds no `$disconnect` route, so the close calls nothing.
            counter("MessageCount", 3.0),
            latency_samples(1.0),
            counter("ClientError", 1.0),
            counter("ExecutionError", 0.0),
            counter("IntegrationError", 0.0),
            // Two calls: the connection request and the frame the api routed nowhere.
            samples("ClientError", 2.0),
            samples("ExecutionError", 2.0),
        ],
    )
    .await;

    // An api whose `$connect` integration refuses the client: the handshake is answered with
    // the status that integration named, and the stage meters the refusal.
    let guarded = given_api(&emulator, case, "guarded-api").await;
    given_route(&emulator, case, &guarded.api_id, "$connect", &refusing, false).await;
    given_stage(&emulator, case, &guarded.api_id, "guard", false).await;

    let refused = handshake_at(case, &guarded.authority, "/guard").await.err();
    assert_eq!(
        refused,
        Some(403),
        "case {case:?}: a connection request its backend refused must be answered with the \
         status that integration named, got {refused:?}"
    );

    reached(
        &emulator,
        case,
        &guarded.api_id,
        "guard",
        &[
            counter("ConnectCount", 1.0),
            counter("MessageCount", 1.0),
            latency_samples(1.0),
            counter("ClientError", 1.0),
            // The integration ANSWERED — it refused the client rather than failing — and a
            // client the backend refused with a client-error status is that one class and no
            // other: neither the route's execution nor the integration earned a count.
            counter("ExecutionError", 0.0),
            counter("IntegrationError", 0.0),
            // One call: the connection request the handshake was refused for.
            samples("IntegrationError", 1.0),
        ],
    )
    .await;

    // An api with NO `$connect` route: the handshake completes, because there is nothing to
    // refuse the connection — and no message is sent to a `$connect` integration, so the
    // stage counts the connection request as a message of its own and `ConnectCount` stays at
    // nothing ("the number of messages sent to the $connect route integration").
    let open = given_api(&emulator, case, "open-api").await;
    given_route(&emulator, case, &open.api_id, "$default", &lifecycle, false).await;
    given_stage(&emulator, case, &open.api_id, "open", false).await;

    let mut socket: ClientSocket = handshake_at(case, &open.authority, "/open")
        .await
        .unwrap_or_else(|status| {
            panic!("case {case:?}: an api with no $connect route must upgrade, got {status}")
        });
    assert!(
        exchange(case, &mut socket, r#"{"action":"anything"}"#)
            .await
            .is_none(),
        "case {case:?}: the one-way $default route sends nothing back"
    );
    closed(case, socket).await;

    reached(
        &emulator,
        case,
        &open.api_id,
        "open",
        &[
            counter("ConnectCount", 0.0),
            // The connection request and the frame; the api holds no `$disconnect` route.
            counter("MessageCount", 2.0),
            // Only the `$default` route's integration answered anything.
            latency_samples(1.0),
            counter("ClientError", 0.0),
            counter("ExecutionError", 0.0),
            counter("IntegrationError", 0.0),
            samples("ClientError", 2.0),
        ],
    )
    .await;
}

/// WHERE a stage states `DetailedMetricsEnabled` — in the `RouteSettings` of one route, or in
/// the `DefaultRouteSettings` every route it does not name is served under — and what each
/// placement makes the stage publish.
struct SeriesScope {
    /// What this row varies, for the failure message.
    what: &'static str,
    /// The stage this row creates, deploys and drives its traffic through.
    stage: &'static str,
    /// `DetailedMetricsEnabled` in the stage's `DefaultRouteSettings`, or `None` for a stage
    /// that names no default route settings at all.
    default_detailed: Option<bool>,
    /// `DetailedMetricsEnabled` in the `RouteSettings` of the routes this row names.
    route_detailed: &'static [(&'static str, bool)],
    /// The routes the stage must publish per-route series for; every other route of the api
    /// must have none.
    own_series: &'static [&'static str],
}

/// What the traffic every row below carries owes EACH route's own series, for a stage that
/// publishes one for that route: the `MessageCount`, the number of `IntegrationLatency`
/// samples, and the `ConnectCount`.
///
/// The connection request is one message sent to the `$connect` route's integration, the
/// two-way frame is a message in and a reply out of its route, and the close is one message of
/// the `$disconnect` route; each of the three integrations answered once.
const PER_ROUTE_TRAFFIC: [(&str, f64, f64, f64); 3] = [
    ("$connect", 1.0, 1.0, 1.0),
    ("reply", 2.0, 1.0, 0.0),
    ("$disconnect", 1.0, 1.0, 0.0),
];

/// Create the stage `row` configures and release a deployment to it, stating
/// `detailedMetricsEnabled` only where that row places it.
async fn given_scoped_stage(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    row: &SeriesScope,
) {
    given_configured_stage(
        emulator,
        case,
        api_id,
        row.stage,
        row.default_detailed,
        row.route_detailed,
    )
    .await;
}

/// Which routes of a stage have series of their own is decided by `DetailedMetricsEnabled` of
/// the route's OWN `RouteSettings` where the stage names them, and by the stage's
/// `DefaultRouteSettings` otherwise — while the stage-wide series, the total across the
/// stage's routes, are published whatever that setting says.
///
/// The rows span where the setting lives: nowhere at all, the stage default alone, one route
/// alone, and one route contradicting the default in each direction. Every row carries the
/// SAME traffic through its own stage of ONE api — a handshake the stage upgrades, one two-way
/// frame answered by its route, and the close — so what differs between the rows is only the
/// dimension sets the datapoints land under.
#[tokio::test]
async fn the_route_settings_that_name_detailed_metrics_decide_which_routes_have_own_series() {
    let case = "DetailedMetricsEnabled placed in a stage's default and in one route's settings";
    let (emulator, engine) = emulator();

    let lifecycle = given_function(&emulator, &engine, case, "ws-scope-lifecycle", OK_HANDLER).await;
    let echo = given_function(&emulator, &engine, case, "ws-scope-echo", ECHO_HANDLER).await;

    let api = given_api(&emulator, case, "detailed-metrics-scope-api").await;
    for (route_key, function, two_way) in [
        ("$connect", &lifecycle, false),
        ("$disconnect", &lifecycle, false),
        ("reply", &echo, true),
    ] {
        given_route(&emulator, case, &api.api_id, route_key, function, two_way).await;
    }

    let rows = [
        SeriesScope {
            what: "a stage that states detailed metrics nowhere",
            stage: "unstated",
            default_detailed: None,
            route_detailed: &[],
            own_series: &[],
        },
        SeriesScope {
            what: "a stage that turns detailed metrics on in its default route settings",
            stage: "default_on",
            default_detailed: Some(true),
            route_detailed: &[],
            own_series: &["$connect", "reply", "$disconnect"],
        },
        SeriesScope {
            what: "one route alone turning detailed metrics on, the stage stating no default",
            stage: "one_route",
            default_detailed: None,
            route_detailed: &[("reply", true)],
            own_series: &["reply"],
        },
        SeriesScope {
            what: "one route turning detailed metrics on against a default that is off",
            stage: "route_over_off",
            default_detailed: Some(false),
            route_detailed: &[("$connect", true)],
            own_series: &["$connect"],
        },
        SeriesScope {
            what: "one route turning detailed metrics off against a default that is on",
            stage: "route_over_on",
            default_detailed: Some(true),
            route_detailed: &[("reply", false)],
            own_series: &["$connect", "$disconnect"],
        },
    ];

    for row in &rows {
        let row_case = format!("{what} (stage {stage:?})", what = row.what, stage = row.stage);
        let case = row_case.as_str();
        given_scoped_stage(&emulator, case, &api.api_id, row).await;

        let path = format!("/{stage}", stage = row.stage);
        let mut socket: ClientSocket = handshake_at(case, &api.authority, &path)
            .await
            .unwrap_or_else(|status| {
                panic!(
                    "case {case:?}: a deployed WEBSOCKET stage must answer the opening \
                     handshake with 101 Switching Protocols, got {status}"
                )
            });
        let replied = exchanged_reply(case, &mut socket, r#"{"action":"reply"}"#).await;
        assert!(
            replied.contains("MESSAGE"),
            "case {case:?}: the two-way route must answer its own event, got {replied:?}"
        );
        closed(case, socket).await;

        // The totals the traffic owes this stage, every one of them a figure no stage publishes
        // by standing still: waited for, because the last call of the connection
        // (`$disconnect`) is served after the client has closed.
        let mut owed = vec![
            counter("ConnectCount", 1.0),
            counter("MessageCount", 4.0),
            latency_samples(3.0),
            // One observation of each error class per CALL the stage served: the connection
            // request, the frame and the close.
            samples("ClientError", 3.0),
        ];
        // The per-route series of the routes this row's settings DO give one, beside them.
        for (route, messages, latencies, connects) in PER_ROUTE_TRAFFIC {
            if !row.own_series.contains(&route) {
                continue;
            }
            owed.push(per_route("MessageCount", route, messages));
            owed.push(per_route("IntegrationLatency", route, latencies));
            if connects > 0.0 {
                owed.push(per_route("ConnectCount", route, connects));
            }
        }
        reached(&emulator, case, &api.api_id, row.stage, &owed).await;

        // Every datapoint of the connection has landed by the time those totals hold — the
        // `$disconnect` call they count is its last — so what a series must NOT hold is read
        // ONCE here rather than waited for: a zero that is asserted after the traffic settled
        // is an absence, while one waited for would hold before the traffic had happened.
        let mut settled: Vec<Expected> = vec![
            counter("ClientError", 0.0),
            counter("ExecutionError", 0.0),
            counter("IntegrationError", 0.0),
        ];
        for (route, messages, latencies, connects) in PER_ROUTE_TRAFFIC {
            let owed_for = |value: f64| {
                if row.own_series.contains(&route) {
                    value
                } else {
                    0.0
                }
            };
            settled.push(per_route("MessageCount", route, owed_for(messages)));
            settled.push(per_route("IntegrationLatency", route, owed_for(latencies)));
            settled.push(per_route("ConnectCount", route, owed_for(connects)));
        }
        for want in &settled {
            let got = observed(&emulator, case, &api.api_id, row.stage, want).await;
            assert!(
                (got - want.value).abs() < 0.5,
                "case {case:?}: the {metric} series of route {route:?} must stand at \
                 {expected} once the stage's totals are in, got {got}",
                metric = want.metric,
                route = want.route,
                expected = want.value
            );
        }
    }
}
