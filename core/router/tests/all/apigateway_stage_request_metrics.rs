// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a deployed REST API stage publishes about the requests it ANSWERED,
//! and the alarm a client pages on 5xx with, end to end over the wire.
//!
//! The rule under test, one level above the recorded case:
//!
//! > **Every request a deployed stage of a REST API answered is published in API Gateway's own
//! > `AWS/ApiGateway` namespace: one observation of `Count`, and one of each error counter —
//! > `4XXError` for the client-error class and `5XXError` for the server-error class — which is
//! > 1 for a request whose status is in that class and 0 for one that is not, beside one
//! > `Latency` sample of how long answering it took and one `IntegrationLatency` sample for
//! > each request whose method's INTEGRATION answered. The ANSWER decides which counter a
//! > request lands in and not what produced it, so a request the api refused itself is counted
//! > like one an integration answered, while a request no stage of a stored api served is
//! > counted nowhere. The series are dimensioned by `ApiName`, and by `ApiName` with `Stage`,
//! > and a method whose serving stage turns `MethodSetting$metricsEnabled` on gets the same
//! > observations again under its own `Method` and `Resource` dimensions beside those two — a
//! > method whose stage does not publishes the stage-level series alone.**
//!
//! Two crates have to agree for that to hold — API Gateway resolves the stage and answers the
//! request, and the metric store the datapoints land in is CloudWatch's, the very store
//! `GetMetricStatistics` and the alarm evaluation read — so the suite lives here, on the
//! emulator as it is assembled. Every assertion reads the outcome back through CloudWatch's
//! own reader, so an observation that did not reach that store cannot pass.
//!
//! The rows differ along what the rule generalises over: the STATUS CLASS the stage answered
//! with (a success, a client error, a server error, a refusal the api words itself), how MANY
//! requests each row drives (a different number per row, so no row's count can stand in for
//! another's), whether a deployed METHOD served the request at all, which STAGE of the api
//! served it, whether that stage turns detailed metrics on for the method, and a stage the api
//! does not hold, which is traffic of no stage.
//!
//! The second test drives the whole chain the series exist for: an alarm over one stage's
//! `5XXError` series settles `OK` while the stage serves successes and crosses into `ALARM`
//! once it serves server errors, which `DescribeAlarmHistory` records as a `StateUpdate` out
//! of `OK` with a reason that states the threshold was crossed — while the alarm over an api
//! that only ever answers successes stays `OK` and records no crossing at all.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every host below is addressed in.
const REGION: &str = "us-east-1";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), which is
/// what routes a call to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace API Gateway publishes what its apis answered in, and the dimensions one of
/// these series is identified by.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_NAME: &str = "ApiName";
const STAGE: &str = "Stage";
const METHOD: &str = "Method";
const RESOURCE: &str = "Resource";

/// The counters and the measurements one answered request is observed in.
const COUNT: &str = "Count";
const CLIENT_ERRORS: &str = "4XXError";
const SERVER_ERRORS: &str = "5XXError";
const LATENCY: &str = "Latency";
const INTEGRATION_LATENCY: &str = "IntegrationLatency";
const CACHE_HITS: &str = "CacheHitCount";
const CACHE_MISSES: &str = "CacheMissCount";

/// The units those series carry: a bare count of requests, and a measured duration.
const COUNT_UNIT: &str = "Count";
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The verb every method below is deployed under.
const VERB: &str = "GET";

/// The two stages the api under test is deployed to, and the name of a stage it does not hold.
const LIVE: &str = "live";
const BETA: &str = "beta";
const GHOST: &str = "ghost";

/// The period the series are read at, in seconds, and how far either side of the TRAFFIC a
/// read covers — generous, because the only thing that decides which window a datapoint falls
/// in is the instant the request was answered at, and the traffic itself takes as long as the
/// machine takes.
const PERIOD: i64 = 60;
const WINDOW_MARGIN_PERIODS: f64 = 2.0;

/// How long ONE request to the deployed stage is given to be answered. A request still open
/// after this was never going to be answered at all, so it is asserted rather than waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// One row: a path, the stage that serves it, how many requests it drives, and what each of
/// them must be answered with and counted into.
struct Row {
    case: &'static str,
    /// The path within the api the row drives, which is also the `Resource` dimension of a
    /// method that publishes its own series.
    path: &'static str,
    /// The stage the row addresses.
    stage: &'static str,
    /// How many requests this row drives.
    requests: usize,
    /// The HTTP status each of them must be answered with.
    status: u16,
    /// Whether the serving stage turns `metricsEnabled` on for this method, which is what
    /// decides whether the method has series of its own.
    detailed: bool,
    /// Whether a DEPLOYED METHOD served the request, which is what decides whether an
    /// integration answered it and so whether it is observed in [`INTEGRATION_LATENCY`].
    through_a_method: bool,
    /// Whether a stage of a stored api served the request at all, which is what decides
    /// whether it is counted anywhere.
    through_a_stage: bool,
}

/// The table. The statuses are what each method's `MOCK` integration answers, the refusal the
/// stage words for a path its deployment holds no method for, and the refusal the endpoint
/// words for a stage the api does not hold.
static ROWS: [Row; 7] = [
    Row {
        case: "a method whose integration answers a success",
        path: "/ok",
        stage: LIVE,
        requests: 3,
        status: 200,
        detailed: false,
        through_a_method: true,
        through_a_stage: true,
    },
    Row {
        case: "a method whose integration answers the server-error class",
        path: "/boom",
        stage: LIVE,
        requests: 2,
        status: 500,
        detailed: false,
        through_a_method: true,
        through_a_stage: true,
    },
    Row {
        case: "a method whose integration answers the client-error class",
        path: "/missing",
        stage: LIVE,
        requests: 4,
        status: 404,
        detailed: false,
        through_a_method: true,
        through_a_stage: true,
    },
    Row {
        case: "a method whose stage turns detailed metrics on",
        path: "/detailed",
        stage: LIVE,
        requests: 5,
        status: 200,
        detailed: true,
        through_a_method: true,
        through_a_stage: true,
    },
    Row {
        case: "a path the deployment holds no method for",
        path: "/nowhere",
        stage: LIVE,
        requests: 6,
        status: 403,
        detailed: false,
        through_a_method: false,
        through_a_stage: true,
    },
    Row {
        case: "the same method served by another stage, which turns detailed metrics off",
        path: "/detailed",
        stage: BETA,
        requests: 7,
        status: 200,
        detailed: false,
        through_a_method: true,
        through_a_stage: true,
    },
    Row {
        case: "a stage the api does not hold",
        path: "/ok",
        stage: GHOST,
        requests: 8,
        status: 403,
        detailed: false,
        through_a_method: false,
        through_a_stage: false,
    },
];

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published stage
    // URL carries none, as a client of one sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One API Gateway control-plane call that has to be accepted with `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One CloudWatch call over its `awsJson1_0` wire that has to answer 200.
async fn monitoring(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let answered = dispatch(
        router,
        "POST",
        &format!("{CLOUDWATCH_SIGNING_NAME}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            (
                "authorization",
                &authorization(CLOUDWATCH_SIGNING_NAME),
            ),
            (
                "x-amz-target",
                &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
            ),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One unsigned request to a deployed stage's invoke URL — the api's own host, no credential,
/// as a client of a published invoke URL sends it. The request must be ANSWERED.
async fn invoke(router: &MultiServiceRouter, case: &str, api: &str, path: &str) -> Answered {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let answered = ::tokio::time::timeout(
        ANSWER_DEADLINE,
        dispatch(router, VERB, &host, path, &[], Bytes::new()),
    )
    .await;
    answered.unwrap_or_else(|_| {
        panic!(
            "case {case:?}: {VERB} {path} must be answered by the stage within \
             {ANSWER_DEADLINE:?}, got no HTTP response at all"
        )
    })
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// One series of this namespace, as a reader names it: the metric, and the dimensions that
/// identify it together.
struct Series {
    metric: &'static str,
    dimensions: Vec<(&'static str, String)>,
}

impl Series {
    /// The api's own series: every request the api answered, whichever stage served it.
    fn of_api(api_name: &str, metric: &'static str) -> Self {
        Self {
            metric,
            dimensions: vec![(API_NAME, api_name.to_string())],
        }
    }

    /// One STAGE's series: the requests that stage answered.
    fn of_stage(api_name: &str, stage: &str, metric: &'static str) -> Self {
        let mut series = Self::of_api(api_name, metric);
        series.dimensions.push((STAGE, stage.to_string()));
        series
    }

    /// One METHOD's own series, which only a method whose stage asks for them has.
    fn of_method(api_name: &str, stage: &str, path: &str, metric: &'static str) -> Self {
        let mut series = Self::of_stage(api_name, stage, metric);
        series.dimensions.push((METHOD, VERB.to_string()));
        series.dimensions.push((RESOURCE, path.to_string()));
        series
    }

    fn wire(&self) -> Value {
        Value::Array(
            self.dimensions
                .iter()
                .map(|(name, value)| json!({ "Name": name, "Value": value }))
                .collect(),
        )
    }
}

/// What one series holds over the window: how many datapoints a client reads, what they sum
/// to, how many observations they aggregate, and which units they carry.
#[derive(Debug, Default)]
struct Held {
    datapoints: usize,
    sum: f64,
    samples: f64,
    units: BTreeSet<String>,
}

/// Read ONE series over the whole window and fold what it holds.
///
/// `since` is the instant the suite started driving traffic, taken before the first request:
/// the window reaches from a margin before it to a margin past now, so every datapoint the
/// traffic produced is inside it however long the traffic took on this machine.
async fn held(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) -> Held {
    let margin = WINDOW_MARGIN_PERIODS * PERIOD as f64;
    let answered = monitoring(
        router,
        case,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": series.metric,
            "Dimensions": series.wire(),
            "StartTime": since - margin,
            "EndTime": now_secs() + margin,
            "Period": PERIOD,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    let mut folded = Held::default();
    for datapoint in answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        folded.datapoints += 1;
        folded.sum += datapoint["Sum"].as_f64().unwrap_or_default();
        folded.samples += datapoint["SampleCount"].as_f64().unwrap_or_default();
        if let Some(unit) = datapoint["Unit"].as_str() {
            folded.units.insert(unit.to_string());
        }
    }
    folded
}

/// Wall-clock now as epoch seconds, the clock the read windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Assert `series` holds exactly `sum` over `samples` observations, each carrying `unit`.
async fn assert_series(
    router: &MultiServiceRouter,
    case: &str,
    series: &Series,
    since: f64,
    sum: f64,
    samples: f64,
    unit: &str,
) {
    let read = held(router, case, series, since).await;
    assert_eq!(
        read.sum, sum,
        "case {case:?}: expected the {} series to sum to {sum}, got {read:?}",
        series.metric
    );
    assert_eq!(
        read.samples, samples,
        "case {case:?}: expected {samples} observations of {}, got {read:?}",
        series.metric
    );
    assert_eq!(
        read.units,
        BTreeSet::from([unit.to_string()]),
        "case {case:?}: expected every datapoint of {} to carry the unit {unit:?}, got {read:?}",
        series.metric
    );
}

/// Assert `series` holds NOTHING AT ALL: not a zero observation, no datapoint.
async fn assert_empty(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) {
    let read = held(router, case, series, since).await;
    assert_eq!(
        read.datapoints, 0,
        "case {case:?}: expected no datapoint of {} at all, got {read:?}",
        series.metric
    );
}

/// One `MOCK` method on its own resource under the api's root, answering `status`.
///
/// A `MOCK` is the one integration type that needs no backend — "for testing without actually
/// invoking the backend" (`IntegrationType`) — and its request template IS what the
/// integration answered, so the `statusCode` that template names is the result the integration
/// responses are selected by: the patterned branch for a result in its class, and the
/// unpatterned `200` branch for every result no pattern covers.
async fn mock_path(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    path_part: &str,
    status: u16,
) {
    let case = format!("the {path_part} method");
    let created = apigw(
        router,
        &case,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    let resource = member(&case, &created, "id");
    let method = format!("/restapis/{api}/resources/{resource}/methods/{VERB}");
    apigw(
        router,
        &case,
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": format!("{{\"statusCode\": {status}}}") },
        })),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        &case,
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "responseTemplates": { "application/json": r#"{"served":"origin"}"# },
        })),
        201,
    )
    .await;
    if status != 200 {
        apigw(
            router,
            &case,
            "PUT",
            &format!("{method}/responses/{status}"),
            Some(json!({})),
            201,
        )
        .await;
        apigw(
            router,
            &case,
            "PUT",
            &format!("{method}/integration/responses/{status}"),
            Some(json!({
                "selectionPattern": format!(r"{}\d{{2}}", status / 100),
                "responseTemplates": { "application/json": r#"{"served":"error"}"# },
            })),
            201,
        )
        .await;
    }
}

/// An api named `name` carrying one `MOCK` method per entry of `paths`, deployed to both
/// stages. Answers the api's id.
async fn deployed_api(
    router: &MultiServiceRouter,
    name: &str,
    paths: &[(&str, u16)],
) -> String {
    let created = apigw(
        router,
        name,
        "POST",
        "/restapis",
        Some(json!({ "name": name })),
        201,
    )
    .await;
    let api = member(name, &created, "id");
    let root = member(name, &created, "rootResourceId");
    for (path, status) in paths {
        mock_path(router, &api, &root, path.trim_start_matches('/'), *status).await;
    }
    for stage in [LIVE, BETA] {
        apigw(
            router,
            name,
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": stage })),
            201,
        )
        .await;
    }
    api
}

/// `UpdateStage` turning `metricsEnabled` on for the method `VERB` on `path` of `stage`.
///
/// The patch path is the setting's published group-and-name under the method path
/// `Stage$methodSettings` is keyed by, with the resource path's separators escaped the way a
/// JSON pointer escapes them.
async fn enable_detailed_metrics(
    router: &MultiServiceRouter,
    api: &str,
    stage: &str,
    path: &str,
) {
    let method_path = path.replace('/', "~1");
    apigw(
        router,
        "detailed metrics",
        "PATCH",
        &format!("/restapis/{api}/stages/{stage}"),
        Some(json!({
            "patchOperations": [{
                "op": "replace",
                "path": format!("/{method_path}/{VERB}/metrics/enabled"),
                "value": "true",
            }],
        })),
        200,
    )
    .await;
}

/// Drive `row`'s requests at `api`, asserting each one is answered with the status the row
/// expects — so a row that counts N requests really made N answered requests.
async fn drive(router: &MultiServiceRouter, api: &str, row: &Row) {
    for ordinal in 1..=row.requests {
        let answered = invoke(
            router,
            row.case,
            api,
            &format!("/{}{}", row.stage, row.path),
        )
        .await;
        assert_eq!(
            answered.status, row.status,
            "case {:?} request {ordinal} of {}: expected HTTP {}, got {} with {:?}",
            row.case,
            row.requests,
            row.status,
            answered.status,
            answered.text()
        );
    }
}

/// What a counter must hold over a set of rows: how many of their requests landed in the class
/// `class` names, and how many requests they drove in all.
fn expected<'r>(
    rows: impl Iterator<Item = &'r Row> + Clone,
    class: Option<u16>,
) -> (f64, f64) {
    let requests: usize = rows.clone().map(|row| row.requests).sum();
    let in_class: usize = rows
        .filter(|row| class.is_some_and(|class| row.status / 100 == class))
        .map(|row| row.requests)
        .sum();
    let counted = if class.is_none() { requests } else { in_class };
    (counted as f64, requests as f64)
}

#[tokio::test]
async fn every_request_a_deployed_rest_stage_answers_is_published_in_the_apis_own_series() {
    let router = emulator();
    let api_name = "stage-metrics-api";
    let quiet_name = "stage-metrics-control-api";
    let paths: Vec<(&str, u16)> = ROWS
        .iter()
        .filter(|row| row.through_a_method)
        .map(|row| (row.path, row.status))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let api = deployed_api(&router, api_name, &paths).await;
    // A SECOND api, deployed the same way, which only ever answers successes: its traffic must
    // stay out of the first api's series and the first api's out of its, because the api is
    // part of every one of these series' identity.
    let quiet = deployed_api(&router, quiet_name, &[("/ok", 200)]).await;
    enable_detailed_metrics(&router, &api, LIVE, "/detailed").await;

    // Taken before the first request and used as the start of every read window below, so a
    // machine that takes minutes to drive the traffic still reads all of it back.
    let since = now_secs();
    for row in &ROWS {
        drive(&router, &api, row).await;
    }
    let quiet_requests = 2;
    for _ in 0..quiet_requests {
        let answered = invoke(&router, quiet_name, &quiet, &format!("/{LIVE}/ok")).await;
        assert_eq!(
            answered.status, 200,
            "case {quiet_name:?}: expected HTTP 200, got {} with {:?}",
            answered.status,
            answered.text()
        );
    }

    // Each row's METHOD: a method whose stage turns detailed metrics on holds exactly that
    // row's requests under its own `Method`/`Resource` dimensions, and every other method —
    // including the SAME method served by the stage that turns them off — holds no datapoint
    // of its own at all.
    for row in &ROWS {
        for (metric, class) in [
            (COUNT, None),
            (CLIENT_ERRORS, Some(4)),
            (SERVER_ERRORS, Some(5)),
        ] {
            let series = Series::of_method(api_name, row.stage, row.path, metric);
            let case = format!("{} read as the method's {metric}", row.case);
            if !row.detailed {
                assert_empty(&router, &case, &series, since).await;
                continue;
            }
            let (sum, samples) = expected([row].into_iter(), class);
            assert_series(&router, &case, &series, since, sum, samples, COUNT_UNIT).await;
        }
    }

    // Each STAGE's series: the requests that stage answered and no others, which is what keeps
    // one stage's traffic out of another's and the requests no stage served out of both.
    for stage in [LIVE, BETA] {
        let rows = ROWS
            .iter()
            .filter(move |row| row.through_a_stage && row.stage == stage);
        for (metric, class) in [
            (COUNT, None),
            (CLIENT_ERRORS, Some(4)),
            (SERVER_ERRORS, Some(5)),
        ] {
            let (sum, samples) = expected(rows.clone(), class);
            let series = Series::of_stage(api_name, stage, metric);
            let case = format!("the {stage} stage read as {metric}");
            assert_series(&router, &case, &series, since, sum, samples, COUNT_UNIT).await;
        }
        // The stage's own latency: one sample per request it answered, in milliseconds.
        let (_, answered_requests) = expected(rows.clone(), None);
        let series = Series::of_stage(api_name, stage, LATENCY);
        let case = format!("the {stage} stage read as {LATENCY}");
        let read = held(&router, &case, &series, since).await;
        assert_eq!(
            read.samples, answered_requests,
            "case {case:?}: expected {answered_requests} latency samples, one per answered \
             request, got {read:?}"
        );
        assert_eq!(
            read.units,
            BTreeSet::from([MILLISECONDS_UNIT.to_string()]),
            "case {case:?}: expected every latency datapoint to carry {MILLISECONDS_UNIT:?}, \
             got {read:?}"
        );
        // The INTEGRATION's own latency: one sample per request a method's integration
        // answered, so the requests the stage refused itself are not among them.
        let answered_by_an_integration: f64 = rows
            .clone()
            .filter(|row| row.through_a_method)
            .map(|row| row.requests as f64)
            .sum();
        let series = Series::of_stage(api_name, stage, INTEGRATION_LATENCY);
        let case = format!("the {stage} stage read as {INTEGRATION_LATENCY}");
        let read = held(&router, &case, &series, since).await;
        assert_eq!(
            read.samples, answered_by_an_integration,
            "case {case:?}: expected {answered_by_an_integration} samples, one per request an \
             integration answered, got {read:?}"
        );
        // Neither cache counter is published for a stage with no cache cluster: a method that
        // caches nothing was never consulted, so there is no hit and no miss to report.
        for metric in [CACHE_HITS, CACHE_MISSES] {
            let series = Series::of_stage(api_name, stage, metric);
            let case = format!("the {stage} stage read as {metric}");
            assert_empty(&router, &case, &series, since).await;
        }
    }

    // The API's own series: every request any stage of it answered, across both stages.
    let served = ROWS.iter().filter(|row| row.through_a_stage);
    for (metric, class) in [
        (COUNT, None),
        (CLIENT_ERRORS, Some(4)),
        (SERVER_ERRORS, Some(5)),
    ] {
        let (sum, samples) = expected(served.clone(), class);
        let series = Series::of_api(api_name, metric);
        let case = format!("the api read as {metric}");
        assert_series(&router, &case, &series, since, sum, samples, COUNT_UNIT).await;
    }

    // The CONTROL api answered its own requests and nothing else: its error counters hold an
    // explicit zero per request, which is what tells "nothing failed" from "this api publishes
    // nothing at all".
    for (metric, sum) in [
        (COUNT, quiet_requests as f64),
        (CLIENT_ERRORS, 0.0),
        (SERVER_ERRORS, 0.0),
    ] {
        let series = Series::of_stage(quiet_name, LIVE, metric);
        let case = format!("the control api read as {metric}");
        assert_series(
            &router,
            &case,
            &series,
            since,
            sum,
            quiet_requests as f64,
            COUNT_UNIT,
        )
        .await;
    }
}

/// How long the alarm test waits for the state the traffic implies, and how often it looks.
///
/// An alarm is evaluated a span after the write that makes its evaluation due, and a datapoint
/// is graded by the evaluation that follows the CLOSE of the period it landed in, so the
/// deadline is several of this alarm's periods with room to spare for a machine under load. An
/// alarm that never crosses fails on what the suite last read instead of hanging it; nothing is
/// asserted about how long the crossing took.
const CROSSED_WITHIN: Duration = Duration::from_secs(180);
/// See [`CROSSED_WITHIN`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The `Period` the alarm below is evaluated on, in seconds: a sub-minute cadence
/// `PutMetricAlarm` admits ("Valid values are 10, 20, 30, and any multiple of 60"), whose
/// window closes promptly while the evaluation range still reaches back past the minute a
/// regular-resolution datapoint is stored at — so the burst the stage served is inside that
/// range however long this machine takes to look again.
const ALARM_PERIOD: i64 = 30;

/// `PutMetricAlarm` over the `5XXError` series of one api's `live` stage, as a client that
/// pages on a 5xx rate writes it.
async fn alarm_over_server_errors(router: &MultiServiceRouter, name: &str, api_name: &str) {
    monitoring(
        router,
        name,
        "PutMetricAlarm",
        json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": SERVER_ERRORS,
            "Dimensions": Series::of_stage(api_name, LIVE, SERVER_ERRORS).wire(),
            "Statistic": "Sum",
            "Period": ALARM_PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanOrEqualToThreshold",
            "TreatMissingData": "notBreaching",
        }),
    )
    .await;
}

/// The `StateValue` `DescribeAlarms` reports for `name`.
async fn alarm_state(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let answered = monitoring(
        router,
        case,
        "DescribeAlarms",
        json!({ "AlarmNames": [name] }),
    )
    .await;
    answered["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `HistoryData` documents of the `StateUpdate` items `DescribeAlarmHistory` holds for
/// `name`, each recording both sides of one transition.
async fn alarm_transitions(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let answered = monitoring(
        router,
        case,
        "DescribeAlarmHistory",
        json!({ "AlarmName": name, "HistoryItemType": "StateUpdate" }),
    )
    .await;
    answered["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            item["HistoryData"]
                .as_str()
                .and_then(|data| serde_json::from_str::<Value>(data).ok())
                .unwrap_or(Value::Null)
        })
        .collect()
}

/// Whether `transition` records the alarm moving from `old` to `new`.
fn moved(transition: &Value, old: Option<&str>, new: &str) -> bool {
    let reported = |member: &str| {
        transition
            .pointer(&format!("/{member}/stateValue"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    reported("newState").as_deref() == Some(new)
        && old.is_none_or(|old| reported("oldState").as_deref() == Some(old))
}

/// The transition out of `OK` into `ALARM` `name` has recorded, once one is there or the
/// deadline is up.
async fn awaited_crossing(router: &MultiServiceRouter, case: &str, name: &str) -> Option<Value> {
    let deadline = Instant::now() + CROSSED_WITHIN;
    loop {
        let crossing = alarm_transitions(router, case, name)
            .await
            .into_iter()
            .find(|transition| moved(transition, Some("OK"), "ALARM"));
        if crossing.is_some() || Instant::now() >= deadline {
            return crossing;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `name`'s state once it is `expected` or the deadline is up, so the test asserts on what it
/// last read rather than hanging.
async fn awaited_state(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) -> String {
    let deadline = Instant::now() + CROSSED_WITHIN;
    loop {
        let state = alarm_state(router, case, name).await;
        if state == expected || Instant::now() >= deadline {
            return state;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

#[tokio::test]
async fn an_alarm_over_a_stages_server_errors_crosses_once_the_stage_serves_them() {
    let router = emulator();
    let paging_name = "paging-api";
    let calm_name = "calm-api";
    let paging = deployed_api(&router, paging_name, &[("/ok", 200), ("/boom", 500)]).await;
    let calm = deployed_api(&router, calm_name, &[("/ok", 200)]).await;
    alarm_over_server_errors(&router, paging_name, paging_name).await;
    alarm_over_server_errors(&router, calm_name, calm_name).await;

    // Both stages serve successes first, which is what settles both alarms in `OK`: each of
    // those requests is an explicit zero of the series the alarms watch.
    for (api, name) in [(&paging, paging_name), (&calm, calm_name)] {
        for _ in 0..3 {
            let answered = invoke(&router, name, api, &format!("/{LIVE}/ok")).await;
            assert_eq!(
                answered.status, 200,
                "case {name:?}: expected HTTP 200 from the stage, got {} with {:?}",
                answered.status,
                answered.text()
            );
        }
        let state = awaited_state(&router, name, name, "OK").await;
        assert_eq!(
            state, "OK",
            "case {name:?}: an alarm over a stage serving successes settles OK, got {state:?}"
        );
    }

    // Then ONE stage serves server errors, and its alarm crosses on them.
    for _ in 0..3 {
        let answered = invoke(&router, paging_name, &paging, &format!("/{LIVE}/boom")).await;
        assert_eq!(
            answered.status, 500,
            "case {paging_name:?}: expected the integration's 500, got {} with {:?}",
            answered.status,
            answered.text()
        );
    }
    let Some(crossing) = awaited_crossing(&router, paging_name, paging_name).await else {
        let state = alarm_state(&router, paging_name, paging_name).await;
        panic!(
            "case {paging_name:?}: the stage served server errors, so its alarm must record a \
             StateUpdate out of OK into ALARM; its history holds none and it reports {state:?}"
        );
    };
    let reason = crossing
        .pointer("/newState/stateReason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        reason.starts_with("Threshold Crossed"),
        "case {paging_name:?}: expected the transition's reason to state the threshold was \
         crossed, got {reason:?}"
    );

    // The api that only ever answered successes is untouched by the other's traffic: no
    // crossing of its own, and the state it still reports is OK.
    let crossings: Vec<Value> = alarm_transitions(&router, calm_name, calm_name)
        .await
        .into_iter()
        .filter(|transition| moved(transition, None, "ALARM"))
        .collect();
    assert!(
        crossings.is_empty(),
        "case {calm_name:?}: an api that answered no server error must record no transition \
         into ALARM, got {crossings:?}"
    );
    let state = alarm_state(&router, calm_name, calm_name).await;
    assert_eq!(
        state, "OK",
        "case {calm_name:?}: expected the control alarm to stay OK, got {state:?}"
    );
}
