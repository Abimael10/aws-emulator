// SPDX-License-Identifier: CC0-1.0

//! What a DEPLOYED REST stage whose methods are RUN by an `AWS_PROXY` Lambda integration
//! serves while its cache cluster holds their answers, and what `FlushStageCache` does to
//! that.
//!
//! RULE: a method request a deployed stage admits is answered by PERFORMING the method's
//! integration — for `AWS_PROXY`, by invoking the Lambda function the integration's `uri`
//! names and serving that run's own envelope — EXCEPT where the stage's cache cluster already
//! holds an unexpired entry for the call's cache key, which answers it with the integration
//! not performed at all ("Specifies whether responses should be cached and returned for
//! requests. A cache cluster must be enabled on the stage for responses to be cached",
//! `MethodSetting$cachingEnabled`; "Specifies the time to live (TTL), in seconds, for cached
//! responses. The higher the TTL, the longer the response will be cached",
//! `MethodSetting$cacheTtlInSeconds`). `FlushStageCache` "flushes a stage's cache", so it
//! empties the cluster and the next request for that same key is served by invoking the
//! function again, however much of the entry's TTL remained. A method whose settings do not
//! enable caching is served by invoking the function for every request.
//!
//! What makes each of those OBSERVABLE is the function's own answer: a Lambda invocation is
//! identified by "a fresh UUID per invocation, which is what `aws_request_id` is on the real
//! runtime", so a handler that answers the request id of the run it is in reports ONE run per
//! distinct value. Two answers carrying the same value were served by one run — the second
//! came from the cluster — and two answers carrying different values are two runs. Nothing
//! here reads a clock to decide that, and nothing waits a span out: the only waits are on
//! conditions another service owes (a stack settling, a function reporting `Active`, a cache
//! cluster reporting `AVAILABLE`).
//!
//! Only a cross-service suite can pin this: the function and its resource policy are AWS
//! Lambda's, the inline handler is archived into a runnable deployment package by
//! CloudFormation, the execution role is IAM's, and the api, the stage, the cache cluster and
//! the integration are API Gateway's. The sibling suites pin the neighbouring halves and this
//! one does not repeat them: [`crate::apigateway_proxy_lambda_ddb_pipeline`] and
//! [`crate::apigateway_proxy_stage_surfaces_its_handlers_envelope`] pin the ENVELOPE a served
//! request is answered from, [`crate::apigateway_token_authorizer_deny_and_access_log`] pins
//! the gate in front of it, and [`crate::apigateway_dynamodb_service_integration`] pins the
//! cache of an `AWS` service integration. What only this suite reaches is the stage CACHE over
//! a backend that is really invoked, and the flush that sends the next request back to it.
//!
//! The rows differ along what the rule generalises over: whether the method's settings cache
//! it at all, which cache entry the request is (two resources, each its own entry), how many
//! requests have already been answered from one entry, and whether the cluster has been
//! flushed since the entry was filled.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::form_encode;

/// Keep this at or above the size the sibling router suites use: assembling the router costs
/// stack.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region of the function, of the role
/// and of the api's own invoke host. It is the Region the shared stack-settling helper
/// ([`wait_for_stack_status`]) reads a stack in.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the grant's `SourceArn` names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that archives the inline handler into a deployment package, the status it has to
/// reach, the function it declares and the role that function runs as.
const STACK: &str = "xsvc-proxy-cache-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const FUNCTION: &str = "xsvc-proxy-cache-handler";
const EXECUTION_ROLE: &str = "xsvc-proxy-cache-runner";

/// The stage the api is deployed to, and the size of the cache cluster it asks for.
const STAGE: &str = "test";
const CACHE_CLUSTER_SIZE: &str = "0.5";

/// The status a cache cluster that is up reports, which is what caching a response needs of it.
const CACHE_CLUSTER_AVAILABLE: &str = "AVAILABLE";

/// The two resources whose methods the stage CACHES, and the one whose method it does not.
/// Each of the first two is its own cache entry, because `PutIntegration` defaults an
/// integration's `cacheNamespace` to its own resource.
const CACHED_PART: &str = "cached";
const OTHER_PART: &str = "other";
const DIRECT_PART: &str = "direct";

/// The time to live the cached methods are given: long enough that no entry filled by this
/// suite can expire while it runs, so an answer that came from the integration again did so
/// because the cluster was EMPTIED and not because the entry aged out.
const TTL_SECONDS: &str = "3000";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is which requests reach the function, not how
/// fast an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work another service owes — a stack reaching `CREATE_COMPLETE`, a
/// function reaching `Active`, a cache cluster reaching `AVAILABLE` — and how long between two
/// looks. Generous on purpose: everything waited for happens in this process, so a loaded host
/// does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The content type the handler's envelope declares, which is what a served answer carries.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The function behind every method: it answers a proxy envelope whose body carries the id of
/// the invocation it is running in, which is what tells one run from another.
const PROXY_HANDLER: &str = r#"
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"invocation": context.aws_request_id}),
    }
"#;

/// One answered request: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the function runs on the Lambda this router
/// mounts, its package is archived by the CloudFormation it mounts, and the stage is served by
/// the API Gateway it mounts.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: the credential scope
    // is what routes an SDK request, and an invoke of the deployed stage carries no credential
    // at all, as a client of that published URL sends none.
    router.set_request_authentication(false);
    router
}

/// The access key id this emulator publishes, which speaks for the account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// A SigV4 `Authorization` whose credential scope names `signing_name`. The signature itself is
/// never checked here.
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
        headers: parts.headers,
        body,
    }
}

/// One awsQuery call of `service` that has to be accepted.
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
        "setup: {service} {action} must answer 200, got {}",
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
        "setup {what:?}: must answer HTTP {expected}, got {}",
        answered.summary()
    );
    answered.json()
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` that `RestApi$disableExecuteApiEndpoint`
/// documents — carrying no credential at all, as a client of an open method sends none.
async fn invoke(router: &MultiServiceRouter, api: &str, path: &str) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{path}"),
        &[],
        Bytes::new(),
    )
    .await
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// Create the execution role the function runs as, trusting the Lambda service to assume it.
/// Answers the ARN IAM holds it under.
async fn create_execution_role(router: &MultiServiceRouter) -> String {
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
        "setup: IAM CreateRole must report the role it made, got {created}"
    );
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// Deploy the stack declaring the function: the inline source is archived into a deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deploy_function(router: &MultiServiceRouter, role_arn: &str) -> String {
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
        "setup: the stack must reach {CREATE_COMPLETE}, got {described}"
    );

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
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup: Lambda must report {FUNCTION:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Grant `apigateway.amazonaws.com` the invocation of the function, scoped by `SourceArn` to
/// the methods of `api` — the grant that admits the integration at all.
async fn grant(router: &MultiServiceRouter, api: &str) {
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
        "setup: AddPermission must answer 201, got {}",
        granted.summary()
    );
}

/// Stand up the api: one `GET` method per [`CACHED_PART`], [`OTHER_PART`] and [`DIRECT_PART`],
/// each on an `AWS_PROXY` integration naming `function_arn`, deployed to [`STAGE`] with a cache
/// cluster. Answers the api's id.
async fn deployed_api(router: &MultiServiceRouter, function_arn: &str) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "proxy-stage-cache-flush" })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &created, "id");
    let resources = apigw(
        router,
        "GetResources",
        "GET",
        &format!("/restapis/{api}/resources"),
        None,
        200,
    )
    .await;
    let root = resources
        .get("item")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .map(|item| as_str("GetResources", item, "id"))
        .expect("the api's root resource");

    for part in [CACHED_PART, OTHER_PART, DIRECT_PART] {
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": part })),
            201,
        )
        .await;
        let resource = as_str("CreateResource", &resource, "id");
        apigw(
            router,
            "PutMethod",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET"),
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
            Some(json!({
                "type": "AWS_PROXY",
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

    grant(router, &api).await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({
            "stageName": STAGE,
            "cacheClusterEnabled": true,
            "cacheClusterSize": CACHE_CLUSTER_SIZE,
        })),
        201,
    )
    .await;

    // Caching on for every method of the stage, and OFF for the one method the rows expect to
    // reach the function every time: a `methodSettings` key naming one method is the setting in
    // force for it, and the whole-stage key is what the others fall back to.
    apigw(
        router,
        "UpdateStage",
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/*/*/caching/enabled", "value": "true" },
                { "op": "replace", "path": "/*/*/caching/ttlInSeconds", "value": TTL_SECONDS },
                {
                    "op": "replace",
                    "path": format!("/~1{DIRECT_PART}/GET/caching/enabled"),
                    "value": "false",
                },
            ],
        })),
        200,
    )
    .await;
    api
}

/// Look at the stage until its cache cluster reports `AVAILABLE`, which is what "a cache
/// cluster must be enabled on the stage for responses to be cached" needs of it. A cluster
/// coming up, and its return from a flush, are transitions the CLOCK owes, so this waits for
/// the CONDITION by looking again rather than for a span to pass.
async fn settled_cluster(router: &MultiServiceRouter, api: &str, case: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let stage = apigw(
            router,
            "GetStage",
            "GET",
            &format!("/restapis/{api}/stages/{STAGE}"),
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
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stage's cache cluster to report \
             {CACHE_CLUSTER_AVAILABLE}, last saw {status:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `FlushStageCache` for the stage, which "flushes a stage's cache".
async fn flush(router: &MultiServiceRouter, api: &str) {
    apigw(
        router,
        "FlushStageCache",
        "DELETE",
        &format!("/restapis/{api}/stages/{STAGE}/cache/data"),
        None,
        202,
    )
    .await;
}

/// The id of the invocation that produced the answer `answered`, asserting it is the function's
/// own envelope that was served: the handler's status, the handler's content type and a body
/// carrying one invocation id in the form a minted request id has.
fn served_invocation(case: &str, answered: &Answered) -> String {
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected the handler's own 200, got {}",
        answered.summary()
    );
    assert_eq!(
        answered.header("content-type").as_deref(),
        Some(JSON_CONTENT_TYPE),
        "case {case:?}: expected the content type the envelope declares, got {:?}",
        answered.header("content-type")
    );
    let body = answered.json();
    let invocation = body
        .get("invocation")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        is_request_id(&invocation),
        "case {case:?}: expected the body to carry the run's own request id, got {}",
        answered.text()
    );
    invocation
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

/// What a row expects of the answer it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The function was invoked for this request: its answer carries a run no earlier request
    /// of this suite was answered from.
    Invoked,
    /// The stage's cluster answered this request from the entry an earlier request filled: its
    /// answer carries that same run.
    FromTheCluster,
}

/// One step of the sequence: a request to the method on the resource `part` expecting
/// `expect`, or the flush that empties the cluster.
enum Step {
    Request {
        case: &'static str,
        part: &'static str,
        expect: Expect,
    },
    Flush {
        case: &'static str,
    },
}

/// The sequence the rule is asserted over. It spans what the rule generalises over: whether the
/// method's settings cache the call, which entry the call is, how many requests one entry has
/// already answered, and whether the cluster was flushed since it was filled.
fn steps() -> Vec<Step> {
    vec![
        Step::Request {
            case: "the first request for a cached method",
            part: CACHED_PART,
            expect: Expect::Invoked,
        },
        Step::Request {
            case: "that same request again, well inside the entry's ttl",
            part: CACHED_PART,
            expect: Expect::FromTheCluster,
        },
        Step::Request {
            case: "a third request the one entry answers",
            part: CACHED_PART,
            expect: Expect::FromTheCluster,
        },
        Step::Request {
            case: "another resource's cached method, which is an entry of its own",
            part: OTHER_PART,
            expect: Expect::Invoked,
        },
        Step::Request {
            case: "that other resource again",
            part: OTHER_PART,
            expect: Expect::FromTheCluster,
        },
        Step::Request {
            case: "a method whose own setting turns caching off",
            part: DIRECT_PART,
            expect: Expect::Invoked,
        },
        Step::Request {
            case: "that uncached method a second time",
            part: DIRECT_PART,
            expect: Expect::Invoked,
        },
        Step::Flush {
            case: "the cluster is flushed with every entry's ttl still running",
        },
        Step::Request {
            case: "the cached method once the cluster has been flushed",
            part: CACHED_PART,
            expect: Expect::Invoked,
        },
        Step::Request {
            case: "the other resource once the cluster has been flushed",
            part: OTHER_PART,
            expect: Expect::Invoked,
        },
        Step::Request {
            case: "the cached method again, answered by the entry the flushed cluster refilled",
            part: CACHED_PART,
            expect: Expect::FromTheCluster,
        },
    ]
}

/// RULE: a request to a cached method of a deployed stage is answered by invoking the
/// `AWS_PROXY` function unless the stage's cluster holds an unexpired entry for it, and
/// `FlushStageCache` empties that cluster so the next request is invoked again.
#[test]
fn a_flushed_stage_cache_sends_the_next_request_back_to_the_proxy_function() {
    std::thread::Builder::new()
        .name("apigateway-proxy-cache-flush".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on
            // (`core/router/src/main.rs`), because one invoke here drives a nested in-process
            // invocation of the function.
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
    let router = emulator();
    let role = create_execution_role(&router).await;
    let function_arn = deploy_function(&router, &role).await;
    let api = deployed_api(&router, &function_arn).await;
    settled_cluster(&router, &api, "the cluster the deployment asked for").await;

    // Which run each resource was last answered from, and every run this suite has seen: an
    // answer that must come from the cluster carries that resource's own held run, and one that
    // must have been invoked carries a run no earlier answer did.
    let mut held: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
    let mut seen: Vec<String> = Vec::new();

    for step in steps() {
        match step {
            Step::Flush { case } => {
                flush(&router, &api).await;
                // A flushed cluster reports `FLUSH_IN_PROGRESS` until it is back up, and a
                // cluster that is not up caches nothing, so the rows after the flush are read
                // once it reports `AVAILABLE` again — which is what lets the last of them
                // observe the entry being refilled.
                settled_cluster(&router, &api, case).await;
                held.clear();
            }
            Step::Request { case, part, expect } => {
                let answered = invoke(&router, &api, &format!("/{part}")).await;
                let invocation = served_invocation(case, &answered);
                match expect {
                    Expect::Invoked => {
                        assert!(
                            !seen.contains(&invocation),
                            "case {case:?}: expected the function to be invoked for this \
                             request, got the run {invocation:?} an earlier answer already \
                             carried"
                        );
                    }
                    Expect::FromTheCluster => {
                        assert_eq!(
                            held.get(part),
                            Some(&invocation),
                            "case {case:?}: expected the run the stage's cluster holds for \
                             {part:?}, got {invocation:?}"
                        );
                    }
                }
                held.insert(part, invocation.clone());
                seen.push(invocation);
            }
        }
    }
}
