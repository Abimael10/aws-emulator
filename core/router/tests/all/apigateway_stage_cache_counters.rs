// SPDX-License-Identifier: CC0-1.0

//! What a deployed REST stage PUBLISHES to CloudWatch about its own response cache, for a
//! method configured through a per-method settings path that names a real resource and verb.
//!
//! The rule under test, one level above any one call:
//!
//! > **A `methodSettings` patch path whose two leading reference tokens name a resource path
//! > and an HTTP METHOD of the api is applied as the setting of THAT method —
//! > `Stage$methodSettings` keys "method paths defined as {resource_path}/{http_method} for an
//! > individual method override, or /\*/\* for overriding all methods in the stage", so the
//! > tokens are filled in by the caller and matched as such rather than compared against the
//! > published template — and the stage then serves and reports that method accordingly: a
//! > request for a method whose settings cache its responses is observed in API Gateway's own
//! > `AWS/ApiGateway` namespace as one `CacheHitCount` and one `CacheMissCount` observation,
//! > 1 in the counter the request belongs to and 0 in the other, where a request the cache
//! > ANSWERED is a hit and one the integration had to answer is a miss — so `FlushStageCache`,
//! > which "flushes a stage's cache", makes the next request a miss again — while a request
//! > for a method whose settings cache nothing is observed in NEITHER counter, and the method's
//! > own `Method`/`Resource` series exist only where that same per-method path turned
//! > `MethodSetting$metricsEnabled` on.**
//!
//! Two crates have to agree for that to hold — API Gateway resolves the stage, decides the
//! method's caching and answers the request, and the metric store the observations land in is
//! CloudWatch's own, the very store `GetMetricStatistics` reads — so the suite lives here, on
//! the emulator as it is ASSEMBLED, and every assertion reads the outcome back through
//! CloudWatch's own reader: an observation that did not reach that store cannot pass.
//!
//! What the neighbouring assembled-router suites already pin, and this one does not repeat:
//! [`crate::apigateway_stage_request_metrics`] pins the request counters, the latencies and the
//! per-method dimensions of a stage with NO cache cluster, where both cache counters are empty;
//! [`crate::apigateway_stage_cache_stale_until_flush`] pins the SERVING side of a held entry —
//! what a client reads while the cluster holds one and after a flush. What only this suite
//! reaches is what a stage REPORTS about that cache: the two counters, per request, as the
//! cache hits, misses and is flushed.
//!
//! The steps differ along what the rule generalises over: what the stage's cache did with the
//! request (it was consulted and held nothing, it answered the request, it was emptied and
//! held nothing again), which METHOD of the one stage the request addressed (one whose
//! settings cache it, one whose settings do not), and the spelling of the per-method patch path
//! that configured it (the resource path's leading separator left to the pointer's own, and
//! escaped into its first reference token) — so a counter that merely counted requests, or one
//! published for every method of the stage, fails here.
//!
//! Nothing here waits a span out or reads a clock to decide an outcome: a cache cluster coming
//! up and returning from a flush are transitions the clock owes, so each is waited for by
//! looking again for the CONDITION until a generous deadline, and the one span a held entry has
//! of its own is set to the top of its published range so no case of this suite can turn on how
//! fast the host runs.
//!
//! The wire harness below — the assembled router, the signed control-plane call, the unsigned
//! invoke of the published stage URL and the reader of one CloudWatch series — is this suite's
//! own, as each suite of this target keeps its own (the sibling API Gateway suites beside it do
//! the same): the helpers are private to their module, and a suite that reached into another's
//! would tie what it drives to what that one happens to expose.

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

/// The two counters a method with a cache is observed in, and the series that say how many
/// requests the stage answered and how many of them an integration answered.
const CACHE_HITS: &str = "CacheHitCount";
const CACHE_MISSES: &str = "CacheMissCount";
const COUNT: &str = "Count";
const INTEGRATION_LATENCY: &str = "IntegrationLatency";

/// The units those series carry: a bare count of requests, and a measured duration.
const COUNT_UNIT: &str = "Count";
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The verb both methods are deployed under, and the stage that serves them.
const VERB: &str = "GET";
const LIVE: &str = "live";

/// The resource whose method the stage CACHES, and the one whose method it does not.
const CACHED_PATH: &str = "/cached";
const PLAIN_PATH: &str = "/plain";

/// The `cacheTtlInSeconds` the cached method is configured with: the top of the published range
/// of the member, so no entry of this suite can age out however long the host takes between two
/// of its requests — what ends a hold here is the flush and nothing else. The expiry is not what
/// any case tests, so it is given every bit of room the setting allows.
const CACHE_TTL_SECONDS: i64 = 3_600;

/// The `cacheClusterStatus` a stage whose cluster is up reports, which is what "a cache cluster
/// must be enabled on the stage for responses to be cached"
/// (`MethodSetting$cachingEnabled`) needs of it.
const CACHE_CLUSTER_AVAILABLE: &str = "AVAILABLE";

/// The cache capacity the stage asks for, one of the modelled `CacheClusterSize` values.
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

/// How long ONE request to the deployed stage is given to be answered. A request still open
/// after this was never going to be answered, so it is asserted rather than waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// What the stage's cache did with one request, which is what decides the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Served {
    /// The cache was consulted for the call and held nothing usable, so the integration
    /// answered it.
    Miss,
    /// The cache HELD the call's response, so the integration was never performed.
    Hit,
    /// The method's settings cache nothing, so the cache was never consulted at all.
    Uncached,
}

/// One step of the traffic: the request it drives and what the cache must make of it.
struct Step {
    case: &'static str,
    /// The path within the api the step addresses.
    path: &'static str,
    /// Whether the stage's cache is FLUSHED before this request is driven.
    flush_first: bool,
    /// What the cache does with it.
    served: Served,
}

/// The table, driven in order against one stage: the first request for the cached method, the
/// repeats its cluster answers, the flush that empties it and the repeat that follows, then the
/// method the settings cache nothing for.
static STEPS: [Step; 7] = [
    Step {
        case: "the first request for the cached method",
        path: CACHED_PATH,
        flush_first: false,
        served: Served::Miss,
    },
    Step {
        case: "a repeat of it, which the held entry answers",
        path: CACHED_PATH,
        flush_first: false,
        served: Served::Hit,
    },
    Step {
        case: "a second repeat, still answered by the held entry",
        path: CACHED_PATH,
        flush_first: false,
        served: Served::Hit,
    },
    Step {
        case: "the first request after the cache was flushed",
        path: CACHED_PATH,
        flush_first: true,
        served: Served::Miss,
    },
    Step {
        case: "a repeat of that one, answered by the entry it filled",
        path: CACHED_PATH,
        flush_first: false,
        served: Served::Hit,
    },
    Step {
        case: "a request for the method whose settings cache nothing",
        path: PLAIN_PATH,
        flush_first: false,
        served: Served::Uncached,
    },
    Step {
        case: "a repeat of that request, performed again",
        path: PLAIN_PATH,
        flush_first: false,
        served: Served::Uncached,
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

/// One unsigned request to the deployed stage's invoke URL — the api's own host, no credential,
/// as a client of a published invoke URL sends it. The request must be ANSWERED.
async fn invoke(router: &MultiServiceRouter, case: &str, api: &str, path: &str) -> Answered {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let stage_path = format!("/{LIVE}{path}");
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
    /// One STAGE's series: the requests that stage answered, whichever method served them.
    fn of_stage(api_name: &str, metric: &'static str) -> Self {
        Self {
            metric,
            dimensions: vec![
                (API_NAME, api_name.to_string()),
                (STAGE, LIVE.to_string()),
            ],
        }
    }

    /// One METHOD's own series, which only a method whose settings turn `metricsEnabled` on
    /// has.
    fn of_method(api_name: &str, path: &str, metric: &'static str) -> Self {
        let mut series = Self::of_stage(api_name, metric);
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

/// One `MOCK` method on its own resource under the api's root, answering 200.
///
/// A `MOCK` is the one integration type that needs no backend — "for testing without actually
/// invoking the backend" (`IntegrationType`) — and its rendered request IS what the integration
/// answered, which is the unit a stage's cluster holds.
async fn mock_method(router: &MultiServiceRouter, api: &str, root: &str, path: &str) {
    let path_part = path.trim_start_matches('/');
    let case = format!("the {path} method");
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
            "responseTemplates": { "application/json": format!(r#"{{"path":"{path}"}}"#) },
        })),
        201,
    )
    .await;
}

/// The stage as a client reads it.
async fn get_stage(router: &MultiServiceRouter, case: &str, api: &str) -> Value {
    apigw(
        router,
        case,
        "GET",
        &format!("/restapis/{api}/stages/{LIVE}"),
        None,
        200,
    )
    .await
}

/// Look at the stage until its cache cluster reports `AVAILABLE` again.
///
/// A cluster coming up, and its return from a flush, are transitions the CLOCK owes, so this
/// waits for the CONDITION by looking again rather than for a span to pass.
async fn settled_cluster(router: &MultiServiceRouter, case: &str, api: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let stage = get_stage(router, case, api).await;
        let status = stage["cacheClusterStatus"].as_str().unwrap_or_default();
        if status == CACHE_CLUSTER_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stage's cache cluster to report \
             {CACHE_CLUSTER_AVAILABLE}, last saw {status:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `FlushStageCache` for the stage, which "flushes a stage's cache", asserting the modelled
/// 202 and waiting out the flush the clock owes.
async fn flush_cache(router: &MultiServiceRouter, case: &str, api: &str) {
    apigw(
        router,
        case,
        "DELETE",
        &format!("/restapis/{api}/stages/{LIVE}/cache/data"),
        None,
        202,
    )
    .await;
    settled_cluster(router, case, api).await;
}

/// The one `methodSettings` entry a stage reports, as `(key, settings)`.
///
/// The entry is read as THE one the stage holds rather than by naming the key it is reported
/// under: which key one method path is held under is a rule of its own, pinned by the owning
/// crate's own suites, and this suite is about the setting that entry carries and what the data
/// plane then does with it.
fn the_one_method_setting(case: &str, stage: &Value) -> (String, Value) {
    let entries: Vec<(String, Value)> = stage["methodSettings"]
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected the stage to report the one method's settings, got {:?}",
        stage["methodSettings"]
    );
    entries.into_iter().next().expect("the one entry")
}

/// An api carrying the two `MOCK` methods, deployed to a stage with a cache cluster that is up,
/// whose CACHED method is configured through per-method patch paths naming that resource and
/// verb. Answers the api's id.
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
    for path in [CACHED_PATH, PLAIN_PATH] {
        mock_method(router, &api, &root, path).await;
    }
    apigw(
        router,
        name,
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({
            "stageName": LIVE,
            "cacheClusterEnabled": true,
            "cacheClusterSize": CACHE_CLUSTER_SIZE,
        })),
        201,
    )
    .await;
    settled_cluster(router, name, &api).await;

    // The per-method settings, written through paths whose `{resourcePath}/{httpMethod}` tokens
    // are FILLED IN with this api's own resource path and verb: the first request spells that
    // resource path with its leading separator left to the pointer's own, the second escapes it
    // into the first reference token — the two spellings a client writes the same method in,
    // each in a request of its OWN, because one request carrying two spellings of one method is
    // the duplicate-method-settings refusal rather than this rule. The method the settings say
    // nothing about is named by no op at all, and no `/*/*` op is sent, so nothing of the stage
    // stands in for it.
    let escaped = CACHED_PATH.replace('/', "~1");
    apigw(
        router,
        "the cached method's caching, unescaped",
        "PATCH",
        &format!("/restapis/{api}/stages/{LIVE}"),
        Some(json!({
            "patchOperations": [{
                "op": "replace",
                "path": format!("{CACHED_PATH}/{VERB}/caching/enabled"),
                "value": "true",
            }],
        })),
        200,
    )
    .await;
    let patched = apigw(
        router,
        "the cached method's TTL and metrics, escaped",
        "PATCH",
        &format!("/restapis/{api}/stages/{LIVE}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": format!("/{escaped}/{VERB}/caching/ttlInSeconds"),
                    "value": CACHE_TTL_SECONDS.to_string(),
                },
                {
                    "op": "replace",
                    "path": format!("/{escaped}/{VERB}/metrics/enabled"),
                    "value": "true",
                },
            ],
        })),
        200,
    )
    .await;

    // What the second update answered, and what a later reader of the stage gets, are the same
    // record: the one entry carries every setting those ops wrote — whichever spelling wrote it
    // — and nothing of the method they never named.
    for (case, stage) in [
        ("the second UpdateStage's own answer", patched),
        (
            "GetStage after it",
            get_stage(router, "the configured stage", &api).await,
        ),
    ] {
        let (key, settings) = the_one_method_setting(case, &stage);
        for (expected_member, expected) in [
            ("cachingEnabled", json!(true)),
            ("metricsEnabled", json!(true)),
            ("cacheTtlInSeconds", json!(CACHE_TTL_SECONDS)),
        ] {
            assert_eq!(
                settings[expected_member], expected,
                "case {case:?}: expected {expected_member} {expected:?} under {key:?}, got \
                 {settings:?}"
            );
        }
    }
    api
}

/// RULE: a per-method settings path whose tokens name a real resource path and HTTP method
/// configures THAT method, and a stage publishes one `CacheHitCount` and one `CacheMissCount`
/// observation per request of a method whose settings cache it — 1 in the counter the request
/// belongs to and 0 in the other, a hit for a request its cluster answered and a miss for one
/// the integration answered, a miss again after a flush — and neither counter for a method
/// whose settings cache nothing.
#[tokio::test]
async fn a_cached_methods_hits_and_misses_are_published_as_the_stage_serves_them() {
    let router = emulator();
    let api_name = "stage-cache-counters-api";
    let api = deployed_api(&router, api_name).await;

    // Taken before the first request and used as the start of every read window below, so a
    // machine that takes minutes to drive the traffic still reads all of it back.
    let since = now_secs();

    for (index, step) in STEPS.iter().enumerate() {
        if step.flush_first {
            flush_cache(&router, step.case, &api).await;
        }
        let answered = invoke(&router, step.case, &api, step.path).await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: expected HTTP 200 from the stage, got {} with {:?}",
            step.case,
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.json()["path"],
            json!(step.path),
            "case {:?}: expected the method's own body, got {:?}",
            step.case,
            answered.text()
        );

        // What every step so far owes the counters: one observation of each per request of the
        // CACHED method, the hit counter summing the requests its cluster answered and the miss
        // counter the requests it did not. The requests of the uncached method are in neither,
        // which is what keeps the two counters to the methods that have a cache at all.
        let seen = &STEPS[..=index];
        let counted = |served: Served| {
            seen.iter().filter(|step| step.served == served).count() as f64
        };
        let hits = counted(Served::Hit);
        let misses = counted(Served::Miss);
        let consulted = hits + misses;
        for (metric, sum) in [(CACHE_HITS, hits), (CACHE_MISSES, misses)] {
            let series = Series::of_stage(api_name, metric);
            let case = format!("{} read as the stage's {metric}", step.case);
            assert_series(
                &router,
                &case,
                &series,
                since,
                sum,
                consulted,
                COUNT_UNIT,
            )
            .await;
        }

        // And the cross-check that the counters report what really happened: the integration's
        // own latency is sampled once per request an INTEGRATION answered, so a request the
        // cluster answered adds no sample to it while a hit is counted, and a miss adds one.
        let answered_by_an_integration = misses + counted(Served::Uncached);
        let series = Series::of_stage(api_name, INTEGRATION_LATENCY);
        let case = format!("{} read as the stage's {INTEGRATION_LATENCY}", step.case);
        let read = held(&router, &case, &series, since).await;
        assert_eq!(
            read.samples, answered_by_an_integration,
            "case {case:?}: expected {answered_by_an_integration} samples, one per request an \
             integration answered, got {read:?}"
        );
        assert_eq!(
            read.units,
            BTreeSet::from([MILLISECONDS_UNIT.to_string()]),
            "case {case:?}: expected every latency datapoint to carry {MILLISECONDS_UNIT:?}, \
             got {read:?}"
        );
    }

    // Every request of either method was answered by the stage, so the stage's own request
    // counter holds all of them: the cache counters above are a subset of this traffic rather
    // than the whole of it.
    let requests = STEPS.len() as f64;
    let series = Series::of_stage(api_name, COUNT);
    assert_series(
        &router,
        "the whole traffic read as the stage's Count",
        &series,
        since,
        requests,
        requests,
        COUNT_UNIT,
    )
    .await;

    // The METHOD's own series, which exist because the same per-method path turned
    // `metricsEnabled` on for it: the cached method holds its own hits and misses under its
    // `Method`/`Resource` dimensions.
    let hits = STEPS
        .iter()
        .filter(|step| step.served == Served::Hit)
        .count() as f64;
    let misses = STEPS
        .iter()
        .filter(|step| step.served == Served::Miss)
        .count() as f64;
    for (metric, sum) in [(CACHE_HITS, hits), (CACHE_MISSES, misses)] {
        let series = Series::of_method(api_name, CACHED_PATH, metric);
        let case = format!("the cached method read as its own {metric}");
        assert_series(
            &router,
            &case,
            &series,
            since,
            sum,
            hits + misses,
            COUNT_UNIT,
        )
        .await;
    }

    // And the method the settings never named has no series of its own at all — neither cache
    // counter and not even the request counter — because a per-method setting is the setting of
    // the method its path names and of no other.
    for metric in [CACHE_HITS, CACHE_MISSES, COUNT] {
        let series = Series::of_method(api_name, PLAIN_PATH, metric);
        let case = format!("the uncached method read as its own {metric}");
        assert_empty(&router, &case, &series, since).await;
    }
}
