// SPDX-License-Identifier: CC0-1.0

//! What a deployed REST stage serves while its CACHE CLUSTER holds a method's answer and the
//! backend that answer came from changes underneath it, and what `FlushStageCache` makes of
//! that.
//!
//! RULE: a method whose stage settings enable caching, on a cluster that is up, is answered
//! from the entry the cluster holds for the call — the integration response an earlier
//! request earned ("Specifies whether responses should be cached and returned for requests. A
//! cache cluster must be enabled on the stage for responses to be cached",
//! `MethodSetting$cachingEnabled`) — for as long as that entry is held ("Specifies the time
//! to live (TTL), in seconds, for cached responses. The higher the TTL, the longer the
//! response will be cached", `MethodSetting$cacheTtlInSeconds`). So a change at the backend
//! the method's integration addresses is not observable through that stage until the entry
//! stops being held: its TTL passes, or `FlushStageCache` — which "flushes a stage's cache"
//! — drops it, and the next request performs the integration again and carries what the
//! backend answers now. A method the settings do not cache performs its integration for every
//! request, so nothing it serves is ever stale.
//!
//! What makes the hold OBSERVABLE is a backend that is itself a deployed stage of this
//! emulator: the front api's method carries a NON-PROXY `HTTP` integration ("for integrating
//! an API method with an HTTP backend", `IntegrationType`) whose `uri` is that backend
//! stage's invoke URL, so the body the front stage serves is the body the backend answered.
//! The BACKEND's own answer is then rewritten (its integration response template renders
//! another marker) and redeployed, while the front api is not reconfigured at all — and the
//! backend stage is read DIRECTLY before and after, so an answer that stayed the same at the
//! front is never a backend that failed to change.
//!
//! Only this target reaches the whole trajectory: the front stage's integration addresses an
//! endpoint of the ASSEMBLED emulator, so the request is carried through the router the
//! services are mounted in rather than through one crate's own service.
//!
//! The rows differ along what the rule generalises over: whether the stage's settings cache
//! the method at all, what ends the hold on a primed entry (`FlushStageCache`, and the TTL
//! passing), the form the integration names the backend stage by (the api's own `execute-api`
//! invoke host, and the prefixed path-style address this emulator announces for a client that
//! resolves no AWS host), and the spelling the `UpdateStage` patch turns caching on in (the
//! method path's leading separator left to the pointer, and escaped inside its reference
//! token).
//!
//! Nothing here waits a span out or reads a clock to decide an outcome: every wait looks
//! again for the CONDITION — a cluster reporting `AVAILABLE`, a stage serving a marker —
//! until a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size the sibling router suites use: assembling the router costs
/// stack.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call here is signed for, and so the Region of each api's own invoke host.
const REGION: &str = "us-east-1";

/// The resource and the stage of the BACKEND api, whose `MOCK` method answers the marker.
const BACKEND_PART: &str = "b";
const BACKEND_STAGE: &str = "b";

/// The resource and the stage of the FRONT api, whose method integrates with that stage.
const FRONT_PART: &str = "c";
const FRONT_STAGE: &str = "test";

/// The cache cluster the front stage asks for, and the status one that is up reports
/// (`CacheClusterStatus`).
const CACHE_CLUSTER_SIZE: &str = "0.5";
const CACHE_CLUSTER_AVAILABLE: &str = "AVAILABLE";

/// How long a look-again wait keeps looking, and how long between two looks. Generous on
/// purpose: everything waited for happens in this process, so a loaded host does not decide
/// the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The endpoint a client on the machine this emulator runs on really dials, which is the host
/// of the path-style address this emulator announces a stage at.
const LOCAL_ENDPOINT: &str = "localhost:9297";

/// The reserved path prefix that address carries ahead of the api id:
/// `/_aws/execute-api/{api-id}/{stage}/{resource-path}`.
const DATA_PLANE_PREFIX: &str = "/_aws/execute-api";

/// The two markers the backend answers: the one the entry is primed with, and the one the
/// backend answers once it has been repointed. They are this suite's own values — what the
/// rule is about is the stale-then-fresh sequence, not the markers.
const FIRST: &str = "v1";
const SECOND: &str = "v2";

/// The body the backend's integration response template renders for a marker, and so the body
/// the front stage serves for as long as it holds that answer.
fn body(marker: &str) -> String {
    format!("{{\"version\":\"{marker}\"}}")
}

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

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine: both apis, the stage cache and the
/// integration that carries one stage's request to another are API Gateway's, reached through
/// the router every service is mounted in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: a control-plane call
    // carries a credential scope so the router can name the target service, and an invoke of
    // a published stage URL carries none, as a client of an open method sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// a control-plane call. The signature itself is never checked here.
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

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    request: Option<Value>,
    expected: u16,
) -> Value {
    let payload = request.map(|value| value.to_string()).unwrap_or_default();
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
        "setup {what:?}: expected HTTP {expected} from {method} {path}, got {}",
        answered.summary()
    );
    answered.json()
}

/// The api's own invoke host, which is the endpoint
/// `RestApi$disableExecuteApiEndpoint` documents ("clients can invoke your API with the
/// default `https://{api_id}.execute-api.{region}.amazonaws.com` endpoint").
fn execute_api_host(api: &str) -> String {
    format!("{api}.execute-api.{REGION}.amazonaws.com")
}

/// One request to a deployed stage, carrying no credential at all, as a client of an open
/// method sends none.
async fn invoke(router: &MultiServiceRouter, host: &str, path: &str) -> Answered {
    dispatch(
        router,
        "GET",
        host,
        path,
        &[("accept", "application/json")],
        Bytes::new(),
    )
    .await
}

/// A required string member of a response body.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {what:?}: no {name:?} in {body}"))
        .to_string()
}

/// Look at `host``path` until it answers 200 with `expected`, and answer the body it served
/// then.
///
/// A poll rather than one read: an entry whose TTL has to pass before the integration is
/// reached again, and a deployment a redeploy owes a stage, are conditions this looks for
/// rather than spans it waits out.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    host: &str,
    path: &str,
    expected: &str,
) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let answered = invoke(router, host, path).await;
        if answered.status == 200 && answered.text() == expected {
            return answered.text();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what} to serve 200 with {expected:?}, last saw {}",
            answered.summary()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look at the front stage until its cache cluster reports `AVAILABLE`, which is what "a cache
/// cluster must be enabled on the stage for responses to be cached" needs of it: a cluster
/// still coming up caches nothing. Answers the stage as it read then.
async fn settled_cluster(router: &MultiServiceRouter, case: &str, api: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let stage = apigw(
            router,
            "GetStage",
            "GET",
            &format!("/restapis/{api}/stages/{FRONT_STAGE}"),
            None,
            200,
        )
        .await;
        let status = stage
            .get("cacheClusterStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == CACHE_CLUSTER_AVAILABLE {
            return stage;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stage's cache cluster to report \
             {CACHE_CLUSTER_AVAILABLE}, last saw {status:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The BACKEND api: `GET /b` on a `MOCK` integration — "for testing without actually invoking
/// the backend" (`IntegrationType`) — whose integration response renders [`FIRST`], deployed
/// to [`BACKEND_STAGE`]. Answers its api id and the id of the resource the method sits on.
async fn backend(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name, "endpointConfiguration": { "types": ["REGIONAL"] } })),
        201,
    )
    .await;
    let api = member(case, &created, "id");
    let root = member(case, &created, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": BACKEND_PART })),
        201,
    )
    .await;
    let resource = member(case, &resource, "id");
    let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\":200}" },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "selectionPattern": "",
            "responseTemplates": { "application/json": body(FIRST) },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": BACKEND_STAGE })),
        201,
    )
    .await;
    (api, resource)
}

/// Rewrite what the BACKEND answers — its integration response renders [`SECOND`] now — and
/// deploy the api again, because "a Deployment resource is a snapshot of your REST API
/// configuration" and a stage serves the deployment it names. The FRONT api is not touched.
async fn repoint(router: &MultiServiceRouter, case: &str, api: &str, resource: &str) {
    apigw(
        router,
        &format!("{case}: UpdateIntegrationResponse"),
        "PATCH",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"),
        Some(json!({
            "patchOperations": [{
                "op": "replace",
                "path": "/responseTemplates/application~1json",
                "value": body(SECOND),
            }],
        })),
        200,
    )
    .await;
    apigw(
        router,
        &format!("{case}: CreateDeployment of the repointed backend"),
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": BACKEND_STAGE })),
        201,
    )
    .await;
}

/// The FRONT api: `GET /c` on a non-proxy `HTTP` integration addressing `backend_uri`, with
/// one integration response for the status the backend answers, deployed to [`FRONT_STAGE`]
/// with a cache cluster. `caching` turns caching on for that method, as a row asks. Answers
/// its api id.
async fn front(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    backend_uri: &str,
    caching: Option<Caching>,
) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name, "endpointConfiguration": { "types": ["REGIONAL"] } })),
        201,
    )
    .await;
    let api = member(case, &created, "id");
    let root = member(case, &created, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": FRONT_PART })),
        201,
    )
    .await;
    let resource = member(case, &resource, "id");
    let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        // `integrationHttpMethod` travels on the wire under the integration record's own
        // member name (`PutIntegration$integrationHttpMethod` carries `jsonName: httpMethod`),
        // which is the verb the backend is called with.
        Some(json!({
            "type": "HTTP",
            "httpMethod": "GET",
            "uri": backend_uri,
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({ "selectionPattern": "" })),
        201,
    )
    .await;
    let deployment = apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        None,
        201,
    )
    .await;
    let deployment = member(case, &deployment, "id");
    apigw(
        router,
        "CreateStage",
        "POST",
        &format!("/restapis/{api}/stages"),
        Some(json!({
            "stageName": FRONT_STAGE,
            "deploymentId": deployment,
            "cacheClusterEnabled": true,
            "cacheClusterSize": CACHE_CLUSTER_SIZE,
        })),
        201,
    )
    .await;
    if let Some(caching) = caching {
        // The method path a `methodSettings` entry is addressed by, in the spelling this row
        // uses: a resource path "begins with a '/'" (`Resource$path`), which a patch pointer
        // either escapes inside its own reference token (`~1`) or leaves to the pointer's own
        // separator. Both name the one method.
        let method_path = if caching.escaped_method_path {
            format!("/~1{FRONT_PART}/GET")
        } else {
            format!("/{FRONT_PART}/GET")
        };
        apigw(
            router,
            "UpdateStage",
            "PATCH",
            &format!("/restapis/{api}/stages/{FRONT_STAGE}"),
            Some(json!({
                "patchOperations": [
                    {
                        "op": "replace",
                        "path": format!("{method_path}/caching/enabled"),
                        "value": "true",
                    },
                    {
                        "op": "replace",
                        "path": format!("{method_path}/caching/ttlInSeconds"),
                        "value": caching.ttl_seconds.to_string(),
                    },
                ],
            })),
            200,
        )
        .await;
    }
    // The cluster the stage asked for has to be up before it caches anything, whatever this
    // row expects of it.
    let stage = settled_cluster(router, case, &api).await;
    let setting = stage
        .get("methodSettings")
        // The key is the method path `Stage$methodSettings` documents: the resource path as
        // ONE reference token, its separators — the leading one included — written as the
        // `~1` escape a path name spells a separator with, then the HTTP method.
        .and_then(|settings| settings.get(format!("~1{FRONT_PART}/GET")))
        .cloned()
        .unwrap_or(Value::Null);
    match caching {
        Some(caching) => {
            assert_eq!(
                setting.get("cachingEnabled").and_then(Value::as_bool),
                Some(true),
                "case {case:?}: expected the stage to report caching enabled for the method, \
                 got {setting:?}"
            );
            assert_eq!(
                setting.get("cacheTtlInSeconds").and_then(Value::as_i64),
                Some(i64::from(caching.ttl_seconds)),
                "case {case:?}: expected the stage to report the time to live the patch set, \
                 got {setting:?}"
            );
        }
        None => assert!(
            setting.is_null(),
            "case {case:?}: expected the stage to report no settings for the method, got \
             {setting:?}"
        ),
    }
    api
}

/// `FlushStageCache` of the front stage, which "flushes a stage's cache".
async fn flush(router: &MultiServiceRouter, case: &str, api: &str) {
    apigw(
        router,
        &format!("{case}: FlushStageCache"),
        "DELETE",
        &format!("/restapis/{api}/stages/{FRONT_STAGE}/cache/data"),
        None,
        202,
    )
    .await;
}

/// What a row turns caching on with: the time to live it asks for, and the spelling the patch
/// addresses the method in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Caching {
    ttl_seconds: u32,
    escaped_method_path: bool,
}

/// The time to live of a row whose entry must still be held when the backend has changed:
/// far longer than this suite takes, so an answer that went stale did so because the entry was
/// held and not because it aged out.
const HELD_TTL: u32 = 300;

/// The time to live of the row whose entry ENDS by aging out, which is the shortest hold the
/// setting can name that holds anything at all ("a `cacheTtlInSeconds` of zero caches
/// nothing").
const BRIEF_TTL: u32 = 1;

/// What a row expects to end the hold the cluster has on the primed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndsTheHold {
    /// `FlushStageCache`, with the entry's own time to live nowhere near passing.
    Flush,
    /// The entry's `cacheTtlInSeconds` passing, with no flush at all.
    TtlPassing,
    /// Nothing is held: the stage's settings do not cache this method.
    NothingIsHeld,
}

/// How a row's integration names the backend stage it addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// The api's own `execute-api` invoke host.
    ExecuteApiHost,
    /// The prefixed path-style address this emulator announces for a client that resolves no
    /// AWS host.
    AnnouncedPath,
}

/// One row: the case, how the integration addresses the backend, what caching the stage's
/// settings turn on for the method, and what ends the hold on the primed entry.
struct Row {
    case: &'static str,
    addressing: Addressing,
    caching: Option<Caching>,
    ends: EndsTheHold,
}

const ROWS: &[Row] = &[
    Row {
        case: "a cached front stage on the backend's own invoke host, flushed",
        addressing: Addressing::ExecuteApiHost,
        caching: Some(Caching {
            ttl_seconds: HELD_TTL,
            escaped_method_path: false,
        }),
        ends: EndsTheHold::Flush,
    },
    Row {
        case: "a cached front stage on the announced path address, flushed",
        addressing: Addressing::AnnouncedPath,
        caching: Some(Caching {
            ttl_seconds: HELD_TTL,
            escaped_method_path: true,
        }),
        ends: EndsTheHold::Flush,
    },
    Row {
        case: "a cached front stage whose entry ages out instead of being flushed",
        addressing: Addressing::ExecuteApiHost,
        caching: Some(Caching {
            ttl_seconds: BRIEF_TTL,
            escaped_method_path: false,
        }),
        ends: EndsTheHold::TtlPassing,
    },
    Row {
        case: "a front stage whose settings cache the method not at all",
        addressing: Addressing::AnnouncedPath,
        caching: None,
        ends: EndsTheHold::NothingIsHeld,
    },
];

/// RULE: a method the stage's settings cache is answered from the entry the cluster holds
/// until that entry stops being held — the TTL passing, or `FlushStageCache` dropping it — so
/// a backend that changed under a primed front stage is not observed through it before then,
/// and a method the settings do not cache performs its integration every time.
#[test]
fn a_cached_stage_serves_a_stale_body_until_the_held_entry_ends() {
    std::thread::Builder::new()
        .name("apigateway-stage-cache-stale".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on
            // (`core/router/src/main.rs`), because one request to the front stage drives a
            // nested in-process request to the backend stage.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("build a multi-thread runtime");
            runtime.block_on(suite());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}

async fn suite() {
    for (index, row) in ROWS.iter().enumerate() {
        // One emulator per row: the apis, the cluster and what it holds are that row's own.
        let router = emulator();
        let case = row.case;

        // 1. The backend, and the proof it serves the first marker on its own.
        let (backend_api, backend_resource) =
            backend(&router, case, &format!("cache-backend-{index}")).await;
        let backend_host = execute_api_host(&backend_api);
        let backend_path = format!("/{BACKEND_STAGE}/{BACKEND_PART}");
        served(
            &router,
            case,
            "the backend stage itself",
            &backend_host,
            &backend_path,
            &body(FIRST),
        )
        .await;

        // 2. The front stage, whose non-proxy HTTP integration addresses that stage.
        let backend_uri = match row.addressing {
            Addressing::ExecuteApiHost => format!("https://{backend_host}{backend_path}"),
            Addressing::AnnouncedPath => format!(
                "https://{LOCAL_ENDPOINT}{DATA_PLANE_PREFIX}/{backend_api}{backend_path}"
            ),
        };
        let front_api = front(
            &router,
            case,
            &format!("cache-front-{index}"),
            &backend_uri,
            row.caching,
        )
        .await;
        let front_host = execute_api_host(&front_api);
        let front_path = format!("/{FRONT_STAGE}/{FRONT_PART}");

        // 3. The front stage serves the backend's body, and a second request serves the very
        // same bytes. For a row the settings do not cache that is the backend answering twice,
        // which is why the discriminator is step 5 and not this.
        let primed = served(
            &router,
            case,
            "the front stage priming its cache",
            &front_host,
            &front_path,
            &body(FIRST),
        )
        .await;
        let repeated = invoke(&router, &front_host, &front_path).await;
        assert_eq!(
            repeated.status, 200,
            "case {case:?}: expected the repeated request to be served 200, got {}",
            repeated.summary()
        );
        assert_eq!(
            repeated.text(),
            primed,
            "case {case:?}: expected the repeated request to carry the primed body {primed:?}, \
             got {:?}",
            repeated.text()
        );

        // 4. The BACKEND answers the second marker now, and says so when read directly.
        repoint(&router, case, &backend_api, &backend_resource).await;
        served(
            &router,
            case,
            "the repointed backend stage itself",
            &backend_host,
            &backend_path,
            &body(SECOND),
        )
        .await;

        // 5. What the front stage serves now, which is what the row is about.
        match row.ends {
            EndsTheHold::Flush => {
                let held = invoke(&router, &front_host, &front_path).await;
                assert_eq!(
                    held.status, 200,
                    "case {case:?}: expected the held entry to be served 200, got {}",
                    held.summary()
                );
                assert_eq!(
                    held.text(),
                    body(FIRST),
                    "case {case:?}: expected the stage to keep serving the entry it holds \
                     {first:?} while its time to live stands, got {served:?}",
                    first = body(FIRST),
                    served = held.text()
                );
                flush(&router, case, &front_api).await;
                let after = invoke(&router, &front_host, &front_path).await;
                assert_eq!(
                    after.status, 200,
                    "case {case:?}: expected the request after the flush to be served 200, got \
                     {}",
                    after.summary()
                );
                assert_eq!(
                    after.text(),
                    body(SECOND),
                    "case {case:?}: expected the flushed cluster to send the next request to \
                     the integration, so {second:?}, got {served:?}",
                    second = body(SECOND),
                    served = after.text()
                );
            }
            EndsTheHold::TtlPassing => {
                // No flush at all: the entry stops being held when its own time to live
                // passes, which this looks for rather than times.
                served(
                    &router,
                    case,
                    "the front stage once the held entry's time to live has passed",
                    &front_host,
                    &front_path,
                    &body(SECOND),
                )
                .await;
            }
            EndsTheHold::NothingIsHeld => {
                let fresh = invoke(&router, &front_host, &front_path).await;
                assert_eq!(
                    fresh.status, 200,
                    "case {case:?}: expected an uncached method to be served 200, got {}",
                    fresh.summary()
                );
                assert_eq!(
                    fresh.text(),
                    body(SECOND),
                    "case {case:?}: expected a method the settings do not cache to perform its \
                     integration, so {second:?}, got {served:?}",
                    second = body(SECOND),
                    served = fresh.text()
                );
            }
        }
    }
}
