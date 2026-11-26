// SPDX-License-Identifier: CC0-1.0

//! What a deployed REST API stage whose methods are RUN by an `AWS_PROXY` Lambda function
//! publishes to CloudWatch about the traffic it answered, over two stages of one api that differ
//! in nothing but their cache CLUSTER.
//!
//! The rule under test, one level above any one stage:
//!
//! > **Every request a deployed stage of a REST API answers — and a deployed stage answers every
//! > request it is sent — is published in API Gateway's own `AWS/ApiGateway` namespace, under the
//! > api's name (`ApiName`) and under that name with the answering stage beside it (`ApiName`
//! > with `Stage`): one `Count`, one observation of each error counter (`4XXError` for the
//! > client-error class, `5XXError` for the server-error class) which is 1 for a request whose
//! > answer is in that class and 0 for one that is not, one `Latency` sample, and one
//! > `IntegrationLatency` sample for each request an INTEGRATION answered. For a method the
//! > stage's cache was consulted for — which needs both a cache cluster on the stage ("a cache
//! > cluster must be enabled on the stage for responses to be cached",
//! > `MethodSetting$cachingEnabled`) and a method whose settings cache it — one `CacheHitCount`
//! > and one `CacheMissCount` observation go beside them, 1 in the counter the request belongs to
//! > and 0 in the other, and a stage lacking the cluster publishes NEITHER. Those two counters
//! > report what was really done with the request rather than how many requests there were: a
//! > miss is a request the function was invoked for and a hit is one it was not, so a stage's
//! > `CacheMissCount` sum and its `IntegrationLatency` sample count are both exactly the number
//! > of distinct RUNS of its backing function, which `FlushStageCache` — it "flushes a stage's
//! > cache" — adds one more of per emptied entry.**
//!
//! Five services have to agree for that to hold — IAM holds the execution role, CloudFormation
//! archives the inline handler into a package Lambda can run, Lambda runs it and identifies each
//! run, API Gateway resolves the stage, decides whether its cluster may answer the request,
//! relays the call and publishes what it answered, and the metric store every assertion reads is
//! CloudWatch's own — so the suite lives here, on the emulator as `build_router` assembles it.
//! Every metric assertion reads the outcome back through `GetMetricStatistics`: an observation
//! that did not reach that store cannot pass.
//!
//! What the neighbouring assembled-router suites pin, and this one does not repeat:
//! [`crate::apigateway_stage_request_metrics`] and
//! [`crate::apigateway_rest_stage_metric_keying`] pin the request counters, the latencies, the
//! per-method dimensions and the keying; [`crate::apigateway_stage_cache_counters`] and
//! [`crate::apigateway_stage_count_and_cache_cluster_gate`] pin the cache counters and the
//! cluster gate over `MOCK` methods, which no backend runs; and
//! [`crate::apigateway_proxy_stage_cache_flush_reinvokes_the_function`] pins which requests reach
//! the function without reading a single series. What only this suite reaches is the two together:
//! that the cache counters a Lambda-backed stage publishes AGREE with the runs of the function —
//! each miss one run, each hit none — so a counter that merely counted requests, or one published
//! for a stage with no cluster, fails here.
//!
//! The rows differ along what the rule generalises over: the status class the handler's own
//! envelope carries (a success, a client error, a server error), how MANY requests each row drives
//! (a different number per row and per stage, so no row's contribution to a counter can stand in
//! for another's), which cache entry a request is (one resource per row, since `PutIntegration`
//! defaults an integration's `cacheNamespace` to its own resource), which STAGE of the one api
//! answered it, and whether the cluster has been flushed since the entry was filled.
//!
//! Nothing here waits a span out or reads a clock to decide an outcome: a stack settling, a
//! function reporting itself `Active` and a cache cluster reporting `AVAILABLE` are transitions
//! their own services owe, so each is waited for by looking again for the CONDITION until a
//! generous deadline, and the one span a held entry has of its own is set far past anything this
//! suite can take, so no case can turn on how fast the host runs.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::form_encode;

/// Keep this at or above the size the sibling router suites use: assembling the router costs
/// stack, and one invoke here drives a nested in-process invocation of the function.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every host below is addressed in, every ARN names and the api's invoke host carries.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the invoke grant's `SourceArn` names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The access key id this emulator publishes, which speaks for the account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), which is
/// what routes a read of the series to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The stack that archives the inline handler into a deployment package, the status it has to
/// reach, the function it declares and the role that function runs as.
const STACK: &str = "xsvc-proxy-cache-series-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const FUNCTION: &str = "xsvc-proxy-cache-series-handler";
const EXECUTION_ROLE: &str = "xsvc-proxy-cache-series-runner";

/// The api under test, and its two stages: one carrying a cache cluster and one carrying none.
/// Both serve the same deployment and both ask for the same caching, so the CLUSTER is the one
/// thing that differs between them.
const API_NAME: &str = "proxy-stage-cache-series-api";
const CACHED_STAGE: &str = "cached";
const PLAIN_STAGE: &str = "plain";

/// The capacity the caching stage asks for — one of the modelled `CacheClusterSize` values — and
/// the status a cluster that is up reports.
const CACHE_CLUSTER_SIZE: &str = "0.5";
const CACHE_CLUSTER_AVAILABLE: &str = "AVAILABLE";

/// The `cacheTtlInSeconds` both stages ask for: far past anything this suite can take, so no
/// entry it fills can age out while it runs and an answer that came from the function again did so
/// because the cluster was EMPTIED. The expiry itself is no case of this suite.
const CACHE_TTL_SECONDS: &str = "3000";

/// The verb every method is deployed under.
const VERB: &str = "GET";

/// The namespace API Gateway publishes what its apis answered in, and the dimensions one of these
/// series is identified by.
const NAMESPACE: &str = "AWS/ApiGateway";
const API_NAME_DIMENSION: &str = "ApiName";
const STAGE_DIMENSION: &str = "Stage";

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

/// The period the series are read at, in seconds, and how far either side of the traffic a read
/// window reaches — generous, because the only thing that decides which window a datapoint falls
/// in is the instant its request was answered at, and the traffic takes as long as the machine
/// takes.
const PERIOD: i64 = 60;
const WINDOW_MARGIN_PERIODS: f64 = 3.0;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the series and not how fast an interpreter gets
/// through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work another service owes — a stack reaching `CREATE_COMPLETE`, a function
/// reaching `Active`, a cache cluster reaching `AVAILABLE` — and how long between two looks.
/// Generous on purpose: nothing here asserts how long a transition took, so a loaded host cannot
/// decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long ONE request to a deployed stage is given to be answered. A request still open after
/// this was never going to be answered at all — the whole of these series rests on a stage that
/// answers — so it is asserted rather than waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(120);

/// The function behind every method: the resource it was called for decides the `statusCode` of
/// the envelope it answers — "API Gateway ... uses the Lambda function's output as the HTTP
/// response body, status code and headers" (API Gateway Developer Guide, *Lambda proxy
/// integration*) — and the body carries the id of the invocation the run is in, which is what
/// tells one run from another.
const PROXY_HANDLER: &str = r#"
import json


def handler(event, context):
    path = event.get("path") or event.get("rawPath") or ""
    status = 200
    if path.endswith("/missing"):
        status = 404
    elif path.endswith("/boom"):
        status = 500
    return {
        "statusCode": status,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"run": context.aws_request_id, "path": path}),
    }
"#;

/// One row of the traffic: a resource of the api — which is a cache entry of its own — the status
/// the handler answers it with, and how many requests the row drives at each of the two stages.
struct Row {
    case: &'static str,
    /// The resource's `pathPart`, which is also the last segment of the path a client asks for.
    part: &'static str,
    /// The status the handler's own envelope carries for it.
    status: u16,
    /// How many requests the row drives at the stage WITH a cluster. The first of them fills the
    /// entry and every later one is answered from it.
    at_cached: usize,
    /// How many it drives at the stage with NO cluster, every one of them a run of the function.
    at_plain: usize,
}

/// The table. The counts differ per row and per stage, so no row's contribution to a counter can
/// stand in for another's, and the statuses span the three classes an envelope can carry.
static ROWS: [Row; 3] = [
    Row {
        case: "a success asked for repeatedly, so one entry answers more than one request",
        part: "ok",
        status: 200,
        at_cached: 4,
        at_plain: 3,
    },
    Row {
        case: "the client-error class, chosen by the handler",
        part: "missing",
        status: 404,
        at_cached: 1,
        at_plain: 2,
    },
    Row {
        case: "the server-error class, chosen by the handler",
        part: "boom",
        status: 500,
        at_cached: 1,
        at_plain: 1,
    },
];

/// The row whose entry is asked for again once the cluster has been FLUSHED: the first, which is
/// the one row more than one request of the cached stage was answered from.
fn flushed_row() -> &'static Row {
    ROWS.first().expect("the table holds a row")
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

/// The emulator as it is ASSEMBLED, on one engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of a published stage URL
    // carries none at all, as a client of an open method sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
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

/// One `awsQuery` call of `service` that has to be accepted, answering its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &format!("{service}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(service)),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup {service} {action}: expected HTTP 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
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
        answered.status,
        expected,
        "setup {what:?}: expected HTTP {expected} from API Gateway, got {}",
        answered.summary()
    );
    answered.json()
}

/// One CloudWatch call over its `awsJson1_0` wire that has to answer 200.
async fn monitoring(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
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
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {}",
        answered.summary()
    );
    answered.json()
}

/// One unsigned request to a deployed stage's own invoke URL — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` that `RestApi$disableExecuteApiEndpoint`
/// documents — as a client of a method whose `authorizationType` is `NONE` makes it.
///
/// The request must be ANSWERED: a stage that leaves it open publishes nothing about it, which is
/// the one way every series below reads empty at once.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    api: &str,
    stage: &str,
    part: &str,
) -> Answered {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let path = format!("/{stage}/{part}");
    ::tokio::time::timeout(
        ANSWER_DEADLINE,
        dispatch(router, VERB, &host, &path, &[], Bytes::new()),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "case {case:?}: {VERB} {path} must be answered by the stage within \
             {ANSWER_DEADLINE:?}, got no HTTP response at all"
        )
    })
}

/// A required, non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: expected a non-empty {member:?}, got {body}"))
}

/// Wall-clock now as epoch seconds, the clock the read windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The execution role the function runs as, trusting the Lambda service to assume it.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert!(
        created.contains(EXECUTION_ROLE),
        "setup CreateRole: expected IAM to report the role it made, got {created}"
    );
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// Deploy the stack declaring the function: the inline source is archived into a deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to exist
/// at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Backend": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": PROXY_HANDLER },
                },
            },
        },
    })
    .to_string();
    accepted_query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup CreateStack: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );

    // The invoke the integration makes needs the function RUNNABLE, which is a transition its own
    // service owes: look until it reports it rather than sleeping a span on its behalf.
    let deadline = Instant::now() + BUDGET;
    let configuration = loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if reported.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break reported;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup GetFunctionConfiguration: expected Lambda to report {FUNCTION:?} Active, got \
         {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Ask `stage` to cache the responses of every method it serves, and for how long.
///
/// The whole-stage `/*/*` key is what every method of the stage falls back to, so one patch asks
/// it of all three rows. Nothing here turns `metricsEnabled` on anywhere: the stage-level series
/// under test are the ones that need no opt-in at all.
async fn ask_for_caching(router: &MultiServiceRouter, api: &str, stage: &str) {
    apigw(
        router,
        &format!("the caching asked of stage {stage:?}"),
        "PATCH",
        &format!("/restapis/{api}/stages/{stage}"),
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/*/*/caching/enabled", "value": "true" },
                {
                    "op": "replace",
                    "path": "/*/*/caching/ttlInSeconds",
                    "value": CACHE_TTL_SECONDS,
                },
            ],
        })),
        200,
    )
    .await;
}

/// Look at `stage` until its cache cluster reports `AVAILABLE`, which is what "a cache cluster
/// must be enabled on the stage for responses to be cached" needs of it.
///
/// A cluster coming up, and its return from a flush, are transitions the CLOCK owes, so this waits
/// for the CONDITION by looking again rather than for a span to pass.
async fn settled_cluster(router: &MultiServiceRouter, case: &str, api: &str, stage: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = apigw(
            router,
            case,
            "GET",
            &format!("/restapis/{api}/stages/{stage}"),
            None,
            200,
        )
        .await;
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

/// `FlushStageCache` for `stage`, which "flushes a stage's cache", asserting the modelled 202 and
/// waiting until the cluster is back up — a cluster that is not up caches nothing.
async fn flush_cache(router: &MultiServiceRouter, case: &str, api: &str, stage: &str) {
    apigw(
        router,
        case,
        "DELETE",
        &format!("/restapis/{api}/stages/{stage}/cache/data"),
        None,
        202,
    )
    .await;
    settled_cluster(router, case, api, stage).await;
}

/// Stand up the api: one open `GET` method per row, each on an `AWS_PROXY` integration naming
/// `function_arn`, deployed to [`CACHED_STAGE`] with a cache cluster and to [`PLAIN_STAGE`] — the
/// same deployment — with none, both asking for the same caching. Answers the api's id.
async fn deployed_api(router: &MultiServiceRouter, function_arn: &str) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({
            "name": API_NAME,
            "endpointConfiguration": { "types": ["REGIONAL"] },
        })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &created, "id");
    let root = as_str("CreateRestApi", &created, "rootResourceId");

    for row in &ROWS {
        let resource = apigw(
            router,
            row.case,
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": row.part })),
            201,
        )
        .await;
        let resource = as_str(row.case, &resource, "id");
        let method = format!("/restapis/{api}/resources/{resource}/methods/{VERB}");
        apigw(
            router,
            row.case,
            "PUT",
            &method,
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        apigw(
            router,
            row.case,
            "PUT",
            &format!("{method}/integration"),
            Some(json!({
                "type": "AWS_PROXY",
                // `integrationHttpMethod` travels under its `jsonName`, which is what
                // `smithy_query(query="operation",operation="PutIntegration")` reports.
                "httpMethod": "POST",
                "uri": format!(
                    "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                     {function_arn}/invocations"
                ),
            })),
            201,
        )
        .await;
    }

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that admits
    // the invocation API Gateway makes as its own service principal.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/*"),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission: expected HTTP 201 from Lambda, got {}",
        granted.summary()
    );

    // The caching stage comes with the deployment; the other is a stage of that SAME deployment,
    // so both serve the very same deployed methods and only the cluster differs.
    let deployment = apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({
            "stageName": CACHED_STAGE,
            "cacheClusterEnabled": true,
            "cacheClusterSize": CACHE_CLUSTER_SIZE,
        })),
        201,
    )
    .await;
    let deployment_id = as_str("CreateDeployment", &deployment, "id");
    apigw(
        router,
        "CreateStage",
        "POST",
        &format!("/restapis/{api}/stages"),
        Some(json!({
            "stageName": PLAIN_STAGE,
            "deploymentId": deployment_id,
            "cacheClusterEnabled": false,
        })),
        201,
    )
    .await;

    for stage in [CACHED_STAGE, PLAIN_STAGE] {
        ask_for_caching(router, &api, stage).await;
    }
    settled_cluster(
        router,
        "the cluster the deployment asked for",
        &api,
        CACHED_STAGE,
    )
    .await;

    // What each stage serves under, as a client reads it back: one of them carries a cluster and
    // the other does not, which is the single difference every assertion below rests on.
    for (stage, cluster) in [(CACHED_STAGE, true), (PLAIN_STAGE, false)] {
        let case = format!("stage {stage:?} as GetStage reports it");
        let read = apigw(
            router,
            &case,
            "GET",
            &format!("/restapis/{api}/stages/{stage}"),
            None,
            200,
        )
        .await;
        assert_eq!(
            read["cacheClusterEnabled"],
            json!(cluster),
            "case {case:?}: expected cacheClusterEnabled {cluster}, got {read}"
        );
        assert!(
            read["methodSettings"]
                .as_object()
                .into_iter()
                .flatten()
                .any(|(_, settings)| settings["cachingEnabled"] == json!(true)),
            "case {case:?}: expected the stage to report method settings that cache its \
             responses, got {read}"
        );
    }
    api
}

/// The id of the RUN that produced `answered`, asserting the function's own envelope was served:
/// the status the row's resource earns, and a body carrying one invocation id in the form a minted
/// request id has, beside the path the run was called for.
fn served_run(case: &str, row: &Row, answered: &Answered) -> String {
    assert_eq!(
        answered.status, row.status,
        "case {case:?}: expected the handler's own HTTP {}, got {}",
        row.status,
        answered.summary()
    );
    let body = answered.json();
    let run = body
        .get("run")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        is_request_id(&run),
        "case {case:?}: expected the body to carry the run's own request id, got {}",
        answered.text()
    );
    let path = body.get("path").and_then(Value::as_str).unwrap_or_default();
    assert!(
        path.ends_with(&format!("/{}", row.part)),
        "case {case:?}: expected the run to have been called for the row's own resource, got {}",
        answered.text()
    );
    run
}

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form of the UUID a Lambda invocation is
/// identified by, which is what makes two distinct values two distinct runs.
fn is_request_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(group, width)| {
                group.len() == width
                    && group
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
}

/// One series of this namespace, as a reader names it.
struct Series {
    metric: &'static str,
    dimensions: Vec<(&'static str, String)>,
}

impl Series {
    /// The api's own series: everything every stage of it answered.
    fn of_api(metric: &'static str) -> Self {
        Self {
            metric,
            dimensions: vec![(API_NAME_DIMENSION, API_NAME.to_string())],
        }
    }

    /// ONE stage's series: the requests that stage answered.
    fn of_stage(stage: &str, metric: &'static str) -> Self {
        let mut series = Self::of_api(metric);
        series.dimensions.push((STAGE_DIMENSION, stage.to_string()));
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
/// how many observations they aggregate, the smallest value any reports, and which units they
/// carry.
#[derive(Debug, Default)]
struct Held {
    datapoints: usize,
    sum: f64,
    samples: f64,
    minimum: f64,
    units: BTreeSet<String>,
}

/// Read ONE series over the whole window through `GetMetricStatistics` and fold what it holds.
///
/// `since` is the instant the suite started driving traffic, taken before the first request: the
/// window reaches from a margin before it to a margin past now, so every datapoint the traffic
/// produced is inside it however long the traffic took on this machine.
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
            "Statistics": ["Sum", "SampleCount", "Minimum"],
        }),
    )
    .await;
    let mut folded = Held {
        minimum: f64::INFINITY,
        ..Held::default()
    };
    for datapoint in answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        folded.datapoints += 1;
        folded.sum += datapoint["Sum"].as_f64().unwrap_or_default();
        folded.samples += datapoint["SampleCount"].as_f64().unwrap_or_default();
        if let Some(minimum) = datapoint["Minimum"].as_f64() {
            folded.minimum = folded.minimum.min(minimum);
        }
        if let Some(unit) = datapoint["Unit"].as_str() {
            folded.units.insert(unit.to_string());
        }
    }
    folded
}

/// Assert the counter `series` sums to `sum` over `samples` observations, each a bare count of
/// requests.
async fn assert_counter(
    router: &MultiServiceRouter,
    case: &str,
    series: &Series,
    since: f64,
    sum: f64,
    samples: f64,
) {
    let read = held(router, case, series, since).await;
    assert_eq!(
        read.sum, sum,
        "case {case:?}: expected the {} series of {:?} to sum to {sum}, got {read:?}",
        series.metric, series.dimensions
    );
    assert_eq!(
        read.samples, samples,
        "case {case:?}: expected {samples} observations of {} under {:?}, got {read:?}",
        series.metric, series.dimensions
    );
    assert_eq!(
        read.units,
        BTreeSet::from([COUNT_UNIT.to_string()]),
        "case {case:?}: expected every datapoint of {} to carry the unit {COUNT_UNIT:?}, got \
         {read:?}",
        series.metric
    );
}

/// Assert the measurement `series` holds exactly `samples` observations, each a duration in
/// milliseconds.
///
/// How LONG answering a request took is what the machine took, so the value itself is no
/// requirement — that there is one observation per request, in the unit the series is expressed in
/// and not below zero, is.
async fn assert_measurement(
    router: &MultiServiceRouter,
    case: &str,
    series: &Series,
    since: f64,
    samples: f64,
) {
    let read = held(router, case, series, since).await;
    assert_eq!(
        read.samples, samples,
        "case {case:?}: expected {samples} observations of {} under {:?}, got {read:?}",
        series.metric, series.dimensions
    );
    assert_eq!(
        read.units,
        BTreeSet::from([MILLISECONDS_UNIT.to_string()]),
        "case {case:?}: expected every datapoint of {} to carry the unit {MILLISECONDS_UNIT:?}, \
         got {read:?}",
        series.metric
    );
    assert!(
        read.minimum.is_finite() && read.minimum >= 0.0,
        "case {case:?}: expected every {} observation under {:?} to be a duration at or above \
         zero, got {read:?}",
        series.metric,
        series.dimensions
    );
}

/// Assert `series` holds NOTHING AT ALL: not a zero observation, no datapoint.
async fn assert_empty(router: &MultiServiceRouter, case: &str, series: &Series, since: f64) {
    let read = held(router, case, series, since).await;
    assert_eq!(
        read.datapoints, 0,
        "case {case:?}: expected no datapoint of {} under {:?} at all, got {read:?}",
        series.metric, series.dimensions
    );
}

/// What one stage's traffic owes its series, accumulated as the rows are driven.
#[derive(Debug, Default)]
struct Traffic {
    /// How many requests the stage answered.
    requests: f64,
    /// How many of them it answered in the client-error class, and how many in the server-error
    /// class.
    client_errors: f64,
    server_errors: f64,
    /// How many of them the stage's cache was consulted for and did NOT answer, so the function
    /// was invoked, and how many it DID answer.
    misses: f64,
    hits: f64,
    /// Every run of the function this stage's answers carried, in order.
    runs: Vec<String>,
}

impl Traffic {
    /// Record one answered request of `row` whose answer carried `run`.
    fn answered(&mut self, row: &Row, run: String) {
        self.requests += 1.0;
        if (400..500).contains(&row.status) {
            self.client_errors += 1.0;
        }
        if (500..600).contains(&row.status) {
            self.server_errors += 1.0;
        }
        self.runs.push(run);
    }

    /// How many DISTINCT runs of the function this stage's traffic was answered from, which is how
    /// many times the function really ran for it.
    fn distinct_runs(&self) -> f64 {
        self.runs.iter().collect::<BTreeSet<&String>>().len() as f64
    }
}

/// RULE: a deployed stage answers every request and publishes it under the api's name and under
/// that name with the stage, and the cache counters of a stage with a cluster report the runs of
/// its backing function — one miss per run, no run for a hit — while a stage with no cluster
/// publishes neither counter.
#[test]
fn a_proxy_backed_stages_cache_series_count_the_runs_of_its_function() {
    std::thread::Builder::new()
        .name("apigateway-proxy-cache-series".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on (`core/router/src/main.rs`),
            // because one invoke here drives a nested in-process invocation of the function.
            let runtime = ::tokio::runtime::Builder::new_multi_thread()
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
    let router = emulator();
    let role_arn = execution_role(&router).await;
    let function_arn = deployed_function(&router, &role_arn).await;
    let api = deployed_api(&router, &function_arn).await;

    // Taken before the first request and used as the start of every read window below, so a
    // machine that takes minutes to drive the traffic still reads all of it back.
    let since = now_secs();

    // The stage WITH a cluster. The first request of a row fills that row's entry — the function
    // runs for it, a MISS — and every later request of the row is answered from the entry, by the
    // very same run, with the function not invoked at all: a HIT.
    let mut cached = Traffic::default();
    for row in &ROWS {
        let mut entry: Option<String> = None;
        for ordinal in 1..=row.at_cached {
            let case = format!(
                "{} at stage {CACHED_STAGE:?}, request {ordinal} of {}",
                row.case, row.at_cached
            );
            let answered = invoke(&router, &case, &api, CACHED_STAGE, row.part).await;
            let run = served_run(&case, row, &answered);
            if let Some(filled) = entry.clone() {
                assert_eq!(
                    run, filled,
                    "case {case:?}: expected the run the stage's cluster holds for this entry, \
                     got {run:?}"
                );
                cached.hits += 1.0;
            } else {
                assert!(
                    !cached.runs.contains(&run),
                    "case {case:?}: expected the function to be invoked to fill the entry, got \
                     the run {run:?} an earlier answer already carried"
                );
                cached.misses += 1.0;
                entry = Some(run.clone());
            }
            cached.answered(row, run);
        }
    }

    // `FlushStageCache` empties the cluster, so the entry one row's later requests were answered
    // from is gone and the next request for it is a MISS again — one more run of the function,
    // with every entry's TTL still running.
    let row = flushed_row();
    let case = format!("{} once the cluster has been flushed", row.case);
    flush_cache(&router, &case, &api, CACHED_STAGE).await;
    let answered = invoke(&router, &case, &api, CACHED_STAGE, row.part).await;
    let run = served_run(&case, row, &answered);
    assert!(
        !cached.runs.contains(&run),
        "case {case:?}: expected the flushed cluster to send this request back to the function, \
         got the run {run:?} an earlier answer already carried"
    );
    cached.misses += 1.0;
    cached.answered(row, run);

    // The stage with NO cluster, serving the same deployment and asking for the same caching:
    // "a cache cluster must be enabled on the stage for responses to be cached", so every one of
    // its requests is a run of the function and none of them is a hit or a miss of anything.
    let mut plain = Traffic::default();
    for row in &ROWS {
        for ordinal in 1..=row.at_plain {
            let case = format!(
                "{} at stage {PLAIN_STAGE:?}, request {ordinal} of {}",
                row.case, row.at_plain
            );
            let answered = invoke(&router, &case, &api, PLAIN_STAGE, row.part).await;
            let run = served_run(&case, row, &answered);
            assert!(
                !plain.runs.contains(&run) && !cached.runs.contains(&run),
                "case {case:?}: expected a stage with no cache cluster to invoke the function for \
                 this request, got the run {run:?} an earlier answer already carried"
            );
            plain.answered(row, run);
        }
    }

    // Each stage's own series hold the traffic THAT stage answered and no other's: every request
    // counted once, each error counter summing the requests of its class and observed for every
    // request, one latency sample per request, and one integration latency sample for each request
    // an integration answered — which for the caching stage is its MISSES alone.
    for (stage, traffic, integration_answers) in [
        (CACHED_STAGE, &cached, cached.misses),
        (PLAIN_STAGE, &plain, plain.requests),
    ] {
        for (metric, sum) in [
            (COUNT, traffic.requests),
            (CLIENT_ERRORS, traffic.client_errors),
            (SERVER_ERRORS, traffic.server_errors),
        ] {
            assert_counter(
                &router,
                &format!("stage {stage:?} read as its own {metric}"),
                &Series::of_stage(stage, metric),
                since,
                sum,
                traffic.requests,
            )
            .await;
        }
        assert_measurement(
            &router,
            &format!("stage {stage:?} read as its own {LATENCY}"),
            &Series::of_stage(stage, LATENCY),
            since,
            traffic.requests,
        )
        .await;
        assert_measurement(
            &router,
            &format!("stage {stage:?} read as its own {INTEGRATION_LATENCY}"),
            &Series::of_stage(stage, INTEGRATION_LATENCY),
            since,
            integration_answers,
        )
        .await;
    }

    // The caching stage's two cache counters: one observation of each per request, the hit counter
    // summing the requests its cluster answered and the miss counter the requests it did not — and
    // the misses are exactly the DISTINCT RUNS of the function its traffic was answered from, so a
    // counter that merely counted requests cannot pass.
    assert_eq!(
        cached.misses,
        cached.distinct_runs(),
        "the caching stage answered {} requests from {} distinct runs of the function, so its \
         misses must be that many, not {}",
        cached.requests,
        cached.distinct_runs(),
        cached.misses
    );
    for (metric, sum) in [(CACHE_HITS, cached.hits), (CACHE_MISSES, cached.misses)] {
        assert_counter(
            &router,
            &format!("stage {CACHED_STAGE:?} read as its own {metric}"),
            &Series::of_stage(CACHED_STAGE, metric),
            since,
            sum,
            cached.requests,
        )
        .await;
    }

    // The stage with no cluster has neither cache counter at all — not a zero observation — while
    // its requests each ran the function, which is what tells a stage whose cache is not working
    // from one that has none.
    assert_eq!(
        plain.distinct_runs(),
        plain.requests,
        "every request of a stage with no cluster is a run of its own: expected {} distinct runs, \
         got {}",
        plain.requests,
        plain.distinct_runs()
    );
    for metric in [CACHE_HITS, CACHE_MISSES] {
        assert_empty(
            &router,
            &format!("stage {PLAIN_STAGE:?} read as its own {metric}"),
            &Series::of_stage(PLAIN_STAGE, metric),
            since,
        )
        .await;
    }

    // And the api's own series hold what BOTH stages answered, so neither stage's series stands in
    // for the api's.
    let requests = cached.requests + plain.requests;
    assert_counter(
        &router,
        &format!("the api read as its own {COUNT}"),
        &Series::of_api(COUNT),
        since,
        requests,
        requests,
    )
    .await;
    for (metric, sum) in [
        (CLIENT_ERRORS, cached.client_errors + plain.client_errors),
        (SERVER_ERRORS, cached.server_errors + plain.server_errors),
    ] {
        assert_counter(
            &router,
            &format!("the api read as its own {metric}"),
            &Series::of_api(metric),
            since,
            sum,
            requests,
        )
        .await;
    }
}
