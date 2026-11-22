// SPDX-License-Identifier: CC0-1.0

//! What a deployed REST API stage publishes about the traffic it answered when NOBODY asked it
//! to publish anything, and which of its series the stage's own cache CLUSTER decides.
//!
//! The rule under test, one level above any one stage:
//!
//! > **Every request a deployed stage of a REST API answered is published in API Gateway's own
//! > `AWS/ApiGateway` namespace as one `Count` observation of the api (`ApiName`) and one of the
//! > stage that answered it (`ApiName` with `Stage`), with nothing to opt into: the per-method
//! > opt-in (`MethodSetting$metricsEnabled`, "specifies whether Amazon CloudWatch metrics are
//! > enabled for this method") governs the method's OWN `Method`/`Resource` series alone, so a
//! > stage that turns it on nowhere still counts all of its traffic. The two cache counters
//! > `CacheHitCount` and `CacheMissCount` are published only for a request the stage's CACHE was
//! > consulted for, which needs both a cache cluster on the stage — "a cache cluster must be
//! > enabled on the stage for responses to be cached" (`MethodSetting$cachingEnabled`) — and a
//! > method whose settings cache it: a stage missing either publishes NEITHER cache series while
//! > still publishing `Count`, and a stage that has both publishes one observation of each per
//! > request, 1 in the counter the request belongs to and 0 in the other. Each stage's series
//! > count the traffic THAT stage answered and no other's, and the two readers of the store —
//! > `GetMetricStatistics` and `GetMetricData` — report the same series.**
//!
//! Two crates have to agree for that to hold — API Gateway resolves the stage, decides whether
//! its cache may answer the request and answers it, and the store the observations land in is
//! CloudWatch's own, the very store a client reads — so the suite lives here, on the emulator as
//! it is ASSEMBLED, and every assertion reads the outcome back through CloudWatch's own readers:
//! an observation that did not reach that store cannot pass.
//!
//! The rows are STAGES of one api, serving one and the same deployed method, and they differ
//! along what the rule generalises over: whether the stage has a cache cluster at all, whether
//! its method settings ask for caching, and how MANY requests each one answers (a different
//! number per row, so no stage's count can stand in for another's). Two rows have exactly one of
//! the two caching conditions each, so a stage whose cache counters followed the SETTING alone,
//! or the CLUSTER alone, fails here — and no row turns the per-method metrics opt-in on, so a
//! stage counter that waited for an opt-in fails too.
//!
//! What the neighbouring assembled-router suites pin and this one does not repeat:
//! [`crate::apigateway_stage_request_metrics`] pins the error counters, the latencies and the
//! per-method dimension set of a stage with no cluster;
//! [`crate::apigateway_stage_cache_counters`] pins hits, misses and `FlushStageCache` on the one
//! stage that caches. What only this suite reaches is the GATE: which of two stages of the same
//! api publishes a cache series at all, while both publish their own `Count`.
//!
//! Nothing here waits a span out or reads a clock to decide an outcome: a cache cluster coming up
//! is a transition the clock owes, so it is waited for by looking again for the CONDITION until a
//! generous deadline, and the one span a held entry has of its own is set to the top of its
//! published range, so no case can turn on how fast the host runs.
//!
//! The wire harness below — the assembled router, the signed control-plane call, the unsigned
//! invoke of a published stage URL and the readers of one CloudWatch series — is this suite's
//! own, as each suite of this target keeps its own.

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

/// The series this suite reads: how many requests the subject answered, the two counters a
/// consulted cache is reported through, and the sample of how long an integration took — which
/// is how a request the cluster answered is told from one it did not.
const COUNT: &str = "Count";
const CACHE_HITS: &str = "CacheHitCount";
const CACHE_MISSES: &str = "CacheMissCount";
const INTEGRATION_LATENCY: &str = "IntegrationLatency";

/// The units those series carry: a bare count of requests, and a measured duration.
const COUNT_UNIT: &str = "Count";
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The verb the one method is deployed under, and the resource path it is on.
const VERB: &str = "GET";
const PATH: &str = "/c";

/// The `cacheTtlInSeconds` the caching stages are configured with: the top of the published
/// range of the member, so no entry of this suite can age out however long the host takes
/// between two requests. The expiry is not what any case tests, so it is given every bit of room
/// the setting allows.
const CACHE_TTL_SECONDS: i64 = 3_600;

/// The `cacheClusterStatus` a stage whose cluster is up reports, and the capacity it asks for —
/// one of the modelled `CacheClusterSize` values.
const CACHE_CLUSTER_AVAILABLE: &str = "AVAILABLE";
const CACHE_CLUSTER_SIZE: &str = "0.5";

/// The period the series are read at, in seconds, and how far either side of the traffic a read
/// covers — generous, because the only thing that decides which window a datapoint falls in is
/// the instant the request was answered at.
const PERIOD: i64 = 60;
const WINDOW_MARGIN_PERIODS: f64 = 2.0;

/// How long a transition the CLOCK owes is given before the suite calls it a failure, and how
/// long between two looks. Generous on purpose: nothing here asserts on how long a transition
/// took, so a loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long ONE request to a deployed stage is given to be answered. A request still open after
/// this was never going to be answered, so it is asserted rather than waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// One row: a STAGE of the one api, how it is configured for caching, and what its traffic owes
/// the series.
struct StageCase {
    /// The stage's name, which is also its `Stage` dimension.
    name: &'static str,
    /// Whether the stage carries a cache CLUSTER, the first of the two caching conditions.
    cluster: bool,
    /// Whether the method's settings on this stage ASK for its responses to be cached, the
    /// second.
    caching_asked: bool,
    /// How many requests this stage answers.
    requests: usize,
    /// How many of them the stage's cache ANSWERED, which is only ever more than none where both
    /// conditions hold.
    hits: usize,
}

impl StageCase {
    /// Whether the stage's cache was consulted for its requests at all, which is what decides
    /// whether the stage has the two cache series.
    fn caches(&self) -> bool {
        self.cluster && self.caching_asked
    }

    /// How many of its requests the cache was consulted for and did NOT answer.
    fn misses(&self) -> usize {
        self.requests - self.hits
    }

    /// How many of its requests an INTEGRATION answered: every one the cache did not.
    fn integration_answers(&self) -> usize {
        self.requests - self.hits
    }
}

/// The table: one api, one deployed method, three stages serving it — one with both caching
/// conditions, one with the setting but no cluster, one with a cluster but no setting — each
/// answering a different number of requests.
static STAGES: [StageCase; 3] = [
    StageCase {
        name: "cached",
        cluster: true,
        caching_asked: true,
        requests: 2,
        hits: 1,
    },
    StageCase {
        name: "ctl",
        cluster: false,
        caching_asked: true,
        requests: 3,
        hits: 0,
    },
    StageCase {
        name: "quiet",
        cluster: true,
        caching_asked: false,
        requests: 1,
        hits: 0,
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
    // scope so the router can name the target service, and the invoke of a published stage URL
    // carries none, as a client of one sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature itself is never checked here.
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

/// One API Gateway control-plane call that has to be answered with `expected`.
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
        "case {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
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
            ("authorization", &authorization(CLOUDWATCH_SIGNING_NAME)),
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

/// One unsigned request to a deployed stage's invoke URL — the api's own host, no credential, as
/// a client of a published invoke URL sends it. The request must be ANSWERED.
async fn invoke(router: &MultiServiceRouter, case: &str, api: &str, stage: &str) -> Answered {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let stage_path = format!("/{stage}{PATH}");
    let answered = ::tokio::time::timeout(
        ANSWER_DEADLINE,
        dispatch(router, VERB, &host, &stage_path, &[], Bytes::new()),
    )
    .await;
    answered.unwrap_or_else(|_| {
        panic!(
            "case {case:?}: {VERB} {stage_path} must be answered by the stage within \
             {ANSWER_DEADLINE:?}, got no HTTP response at all"
        )
    })
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// One series of this namespace, as a reader names it: the metric, and the dimensions that
/// identify it together.
struct Series {
    metric: &'static str,
    dimensions: Vec<(&'static str, String)>,
}

impl Series {
    /// The API's own series: everything every stage of it answered.
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

    /// One METHOD's own series on one stage, which only a method whose settings turn
    /// `metricsEnabled` on has.
    fn of_method(api_name: &str, stage: &str, metric: &'static str) -> Self {
        let mut series = Self::of_stage(api_name, stage, metric);
        series.dimensions.push((METHOD, VERB.to_string()));
        series.dimensions.push((RESOURCE, PATH.to_string()));
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

/// What one series holds over the window: how many datapoints a client reads, what they sum to,
/// how many observations they aggregate, and which units they carry.
#[derive(Debug, Default)]
struct Held {
    datapoints: usize,
    sum: f64,
    samples: f64,
    units: BTreeSet<String>,
}

/// Wall-clock now as epoch seconds, the clock the read windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// How far either side of the traffic a read window reaches.
fn margin() -> f64 {
    WINDOW_MARGIN_PERIODS * PERIOD as f64
}

/// Read ONE series over the whole window through `GetMetricStatistics` and fold what it holds.
///
/// `since` is the instant the suite started driving traffic, taken before the first request: the
/// window reaches from a margin before it to a margin past now, so every datapoint the traffic
/// produced is inside it however long the traffic took on this machine.
async fn held(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) -> Held {
    let answered = monitoring(
        router,
        case,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": series.metric,
            "Dimensions": series.wire(),
            "StartTime": since - margin(),
            "EndTime": now_secs() + margin(),
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

/// What the OTHER reader of the same store — `GetMetricData`, which a dashboard reads a series
/// through — reports the series summing to over the same window.
async fn graphed(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) -> f64 {
    let answered = monitoring(
        router,
        case,
        "GetMetricData",
        json!({
            "MetricDataQueries": [{
                "Id": "driven",
                "MetricStat": {
                    "Metric": {
                        "Namespace": NAMESPACE,
                        "MetricName": series.metric,
                        "Dimensions": series.wire(),
                    },
                    "Period": PERIOD,
                    "Stat": "Sum",
                },
            }],
            "StartTime": since - margin(),
            "EndTime": now_secs() + margin(),
        }),
    )
    .await;
    answered["MetricDataResults"]
        .as_array()
        .and_then(|results| results.first())
        .and_then(|result| result.get("Values"))
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_f64).sum())
        .unwrap_or_default()
}

/// Assert `series` holds exactly `sum` over `samples` observations, each carrying `unit`, and
/// that the store's two readers agree on what it sums to.
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
    let graphed = graphed(router, case, series, since).await;
    assert_eq!(
        graphed, sum,
        "case {case:?}: expected GetMetricData to report the {} series summing to {sum} as \
         GetMetricStatistics does, got {graphed}",
        series.metric
    );
}

/// Assert `series` holds NOTHING AT ALL: not a zero observation, no datapoint — through either
/// reader of the store.
async fn assert_empty(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) {
    let read = held(router, case, series, since).await;
    assert_eq!(
        read.datapoints, 0,
        "case {case:?}: expected no datapoint of {} at all, got {read:?}",
        series.metric
    );
    let graphed = graphed(router, case, series, since).await;
    assert_eq!(
        graphed, 0.0,
        "case {case:?}: expected GetMetricData to report nothing for {} either, got {graphed}",
        series.metric
    );
}

/// One `MOCK` method on its own resource under the api's root, answering 200.
///
/// A `MOCK` is the one integration type that needs no backend — "for testing without actually
/// invoking the backend" (`IntegrationType`) — and its rendered response IS what the integration
/// answered, which is the unit a stage's cluster holds.
async fn mock_method(router: &MultiServiceRouter, api: &str, root: &str) {
    let path_part = PATH.trim_start_matches('/');
    let case = format!("the {PATH} method");
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
            "requestTemplates": { "application/json": r#"{"statusCode":200}"# },
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
            "responseTemplates": { "application/json": format!(r#"{{"path":"{PATH}"}}"#) },
        })),
        201,
    )
    .await;
}

/// One stage as a client reads it.
async fn get_stage(router: &MultiServiceRouter, case: &str, api: &str, stage: &str) -> Value {
    apigw(
        router,
        case,
        "GET",
        &format!("/restapis/{api}/stages/{stage}"),
        None,
        200,
    )
    .await
}

/// Look at the stage until its cache cluster reports `AVAILABLE`.
///
/// A cluster coming up is a transition the CLOCK owes, so this waits for the CONDITION by looking
/// again rather than for a span to pass.
async fn settled_cluster(router: &MultiServiceRouter, case: &str, api: &str, stage: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = get_stage(router, case, api, stage).await;
        let status = read["cacheClusterStatus"].as_str().unwrap_or_default();
        if status == CACHE_CLUSTER_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected stage {stage:?} to report its cache cluster \
             {CACHE_CLUSTER_AVAILABLE}, last saw {status:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Ask `stage` to cache the one method's responses, through a per-method patch path whose
/// `{resourcePath}/{httpMethod}` tokens are FILLED IN with this api's own resource path and verb,
/// the resource path's leading separator escaped into the pointer's first reference token.
///
/// Nothing here turns `metricsEnabled` on anywhere: the stage-level series under test are the
/// ones that need no opt-in at all.
async fn ask_for_caching(router: &MultiServiceRouter, api: &str, stage: &str) {
    let escaped = PATH.replace('/', "~1");
    let case = format!("the caching asked of stage {stage:?}");
    apigw(
        router,
        &case,
        "PATCH",
        &format!("/restapis/{api}/stages/{stage}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": format!("/{escaped}/{VERB}/caching/enabled"),
                    "value": "true",
                },
                {
                    "op": "replace",
                    "path": format!("/{escaped}/{VERB}/caching/ttlInSeconds"),
                    "value": CACHE_TTL_SECONDS.to_string(),
                },
            ],
        })),
        200,
    )
    .await;
}

/// Whether the stage reports the one method setting as CACHING its responses, read back through
/// `GetStage`: the entry is read as the one the stage holds rather than by naming the key it is
/// reported under, because which key one method path is held under is a rule of its own, pinned
/// by the owning crate's own suites.
fn reports_caching(stage: &Value) -> bool {
    stage["methodSettings"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(_, settings)| settings["cachingEnabled"] == json!(true))
}

/// The api of [`STAGES`], carrying the one `MOCK` method and deployed to every row's stage, each
/// configured as its row asks and read back through `GetStage`. Answers the api's id.
async fn deployed_api(router: &MultiServiceRouter, name: &str) -> String {
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
    mock_method(router, &api, &root).await;

    // The FIRST row's stage comes with the deployment; the rest are stages of that same
    // deployment, so every row serves the very same deployed method and only its own caching
    // configuration and traffic differ.
    let (first, rest) = STAGES
        .split_first()
        .unwrap_or_else(|| panic!("setup {name}: the table holds at least one stage"));
    let deployment = apigw(
        router,
        name,
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({
            "stageName": first.name,
            "cacheClusterEnabled": first.cluster,
            "cacheClusterSize": CACHE_CLUSTER_SIZE,
        })),
        201,
    )
    .await;
    let deployment_id = member(name, &deployment, "id");
    for stage in rest {
        let mut body = json!({
            "stageName": stage.name,
            "deploymentId": deployment_id,
            "cacheClusterEnabled": stage.cluster,
        });
        if stage.cluster {
            body["cacheClusterSize"] = json!(CACHE_CLUSTER_SIZE);
        }
        apigw(
            router,
            name,
            "POST",
            &format!("/restapis/{api}/stages"),
            Some(body),
            201,
        )
        .await;
    }

    for stage in &STAGES {
        if stage.caching_asked {
            ask_for_caching(router, &api, stage.name).await;
        }
        if stage.cluster {
            settled_cluster(router, "the stage's cluster", &api, stage.name).await;
        }
        // The configuration each row serves under, as a client reads it back: whether the stage
        // carries a cluster at all, and whether the one method setting asks for caching.
        let case = format!("stage {:?} as GetStage reports it", stage.name);
        let read = get_stage(router, &case, &api, stage.name).await;
        assert_eq!(
            read["cacheClusterEnabled"],
            json!(stage.cluster),
            "case {case:?}: expected cacheClusterEnabled {}, got {read}",
            stage.cluster
        );
        assert_eq!(
            reports_caching(&read),
            stage.caching_asked,
            "case {case:?}: expected the method settings to ask for caching: {}, got {read}",
            stage.caching_asked
        );
    }
    api
}

/// RULE: every request a deployed REST stage answers is published as one `Count` observation of
/// the api and one of that stage, with no opt-in anywhere, while the two cache counters are
/// published only for a stage that has BOTH a cache cluster and a method whose settings cache it
/// — a stage missing either publishes neither cache series and still publishes `Count` — and the
/// store's two readers report the same series.
#[tokio::test]
async fn a_stages_count_needs_no_opt_in_and_its_cache_series_need_a_cluster() {
    let router = emulator();
    let api_name = "stage-count-cache-gate-api";
    let api = deployed_api(&router, api_name).await;

    // Taken before the first request and used as the start of every read window below, so a
    // machine that takes minutes to drive the traffic still reads all of it back.
    let since = now_secs();

    for stage in &STAGES {
        for request in 0..stage.requests {
            let case = format!(
                "request {} of {} to stage {:?}",
                request + 1,
                stage.requests,
                stage.name
            );
            let answered = invoke(&router, &case, &api, stage.name).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected HTTP 200 from the stage, got {} with {:?}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.json()["path"],
                json!(PATH),
                "case {case:?}: expected the method's own body, got {:?}",
                answered.text()
            );
        }
    }

    for stage in &STAGES {
        let requests = stage.requests as f64;

        // What needed no opt-in: the stage's own request counter holds every request it
        // answered, and no row of this suite turned `metricsEnabled` on anywhere.
        assert_series(
            &router,
            &format!("stage {:?} read as its own {COUNT}", stage.name),
            &Series::of_stage(api_name, stage.name, COUNT),
            since,
            requests,
            requests,
            COUNT_UNIT,
        )
        .await;
        assert_empty(
            &router,
            &format!("stage {:?} read as the method's own {COUNT}", stage.name),
            &Series::of_method(api_name, stage.name, COUNT),
            since,
        )
        .await;

        // And the gate: the two cache counters exist for the stage that has both caching
        // conditions and for no other, one observation of each per request of the cached method.
        if stage.caches() {
            for (metric, sum) in [
                (CACHE_HITS, stage.hits as f64),
                (CACHE_MISSES, stage.misses() as f64),
            ] {
                assert_series(
                    &router,
                    &format!("stage {:?} read as its {metric}", stage.name),
                    &Series::of_stage(api_name, stage.name, metric),
                    since,
                    sum,
                    requests,
                    COUNT_UNIT,
                )
                .await;
            }
        } else {
            for metric in [CACHE_HITS, CACHE_MISSES] {
                assert_empty(
                    &router,
                    &format!("stage {:?} read as its {metric}", stage.name),
                    &Series::of_stage(api_name, stage.name, metric),
                    since,
                )
                .await;
            }
        }

        // The cross-check that the counters report what really happened rather than counting
        // requests: the integration's own latency is sampled once per request an INTEGRATION
        // answered, so a request the cluster answered adds no sample to it.
        let case = format!("stage {:?} read as its {INTEGRATION_LATENCY}", stage.name);
        let read = held(
            &router,
            &case,
            &Series::of_stage(api_name, stage.name, INTEGRATION_LATENCY),
            since,
        )
        .await;
        let answered_by_an_integration = stage.integration_answers() as f64;
        assert_eq!(
            read.samples, answered_by_an_integration,
            "case {case:?}: expected {answered_by_an_integration} samples, one per request an \
             integration answered, got {read:?}"
        );
        assert_eq!(
            read.units,
            BTreeSet::from([MILLISECONDS_UNIT.to_string()]),
            "case {case:?}: expected every latency datapoint to carry {MILLISECONDS_UNIT:?}, got \
             {read:?}"
        );
    }

    // The API's own series, which every stage of it contributes to: all of the traffic under
    // `Count`, and under the cache counters exactly the requests of the stage whose cache was
    // consulted — so a stage that publishes no cache series of its own adds nothing to the api's
    // either.
    let total: usize = STAGES.iter().map(|stage| stage.requests).sum();
    assert_series(
        &router,
        &format!("the whole api read as its {COUNT}"),
        &Series::of_api(api_name, COUNT),
        since,
        total as f64,
        total as f64,
        COUNT_UNIT,
    )
    .await;
    let consulted: usize = STAGES
        .iter()
        .filter(|stage| stage.caches())
        .map(|stage| stage.requests)
        .sum();
    for (metric, sum) in [
        (
            CACHE_HITS,
            STAGES
                .iter()
                .filter(|stage| stage.caches())
                .map(|stage| stage.hits)
                .sum::<usize>(),
        ),
        (
            CACHE_MISSES,
            STAGES
                .iter()
                .filter(|stage| stage.caches())
                .map(StageCase::misses)
                .sum::<usize>(),
        ),
    ] {
        assert_series(
            &router,
            &format!("the whole api read as its {metric}"),
            &Series::of_api(api_name, metric),
            since,
            sum as f64,
            consulted as f64,
            COUNT_UNIT,
        )
        .await;
    }
}
