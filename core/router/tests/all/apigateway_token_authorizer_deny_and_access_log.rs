// SPDX-License-Identifier: CC0-1.0

//! What a client of a DEPLOYED REST stage is answered when the method behind it is gated by a
//! `TOKEN` Lambda authorizer, whether the backing `AWS_PROXY` function RAN, and what the
//! stage's own access log records about it — asserted on the emulator as it is ASSEMBLED.
//!
//! RULE: **a deployed method whose `authorizationType` is `CUSTOM` is decided by the POLICY
//! the Lambda function of its `Authorizer` returned for the value the request carried at the
//! authorizer's `identitySource`, evaluated against the ARN of the method the request
//! addressed, and every request is answered synchronously at the edge: a policy allowing
//! `execute-api:Invoke` on that method hands the request to the integration and the client is
//! served what the integration produced; a policy that explicitly DENIES it is refused `403`
//! with API Gateway's explicit-deny sentence and the `AccessDeniedException` class named in
//! `x-amzn-errortype`; a request carrying nothing at the identity source is refused `401`
//! `{"message":"Unauthorized"}`. For every refused request the integration is never performed
//! — nothing of the backend runs, which is observable as the absence of the line the backend
//! prints from its own CloudWatch Logs group — and whichever way the request was answered the
//! stage writes ONE access-log record for it into the destination its `accessLogSettings`
//! name, carrying that answer's status, the request id the response reported and the
//! STAGE-QUALIFIED path the client addressed.**
//!
//! `Method$authorizationType` gives `CUSTOM` "for using a custom authorizer" and
//! `Authorizer$authorizerUri` is "the authorizer's Uniform Resource Identifier (URI) … For
//! `TOKEN` or `REQUEST` authorizers, this must be a well-formed Lambda function URI", so the
//! decision is a function's RETURN VALUE; `Authorizer$authorizerResultTtlInSeconds` "if it
//! equals 0, authorization caching is disabled", so every row's own token reaches the
//! function. `Stage$accessLogSettings` carries "the Amazon Resource Name (ARN) of the
//! CloudWatch Logs log group" a record is written to and the `format` it is rendered by.
//!
//! Five crates have to agree for that to hold — IAM holds the execution role and the
//! account's CloudWatch role, CloudFormation archives the inline handlers into packages this
//! emulator can run, Lambda runs both functions and delivers each run's log, CloudWatch Logs
//! holds the groups every piece of evidence is read from, and API Gateway resolves the stage,
//! takes the decision and delivers the access-log record — so the suite lives here rather
//! than in any one of them.
//!
//! What the neighbouring assembled-router suites already pin, and this one does not repeat:
//! [`crate::apigateway_stage_access_log_destination_delivery`] pins the DELIVERY itself — one
//! event per answered request into the destination the `accessLogSettings` name, and the
//! stream and page readings of it — and
//! [`crate::apigateway_cognito_authorizer_spares_the_proxy_lambda`] pins the same
//! refused-leg-spares-the-backend shape for a `COGNITO_USER_POOLS` gate. What only this suite
//! reaches is the LAMBDA authorizer's own decision on the assembled router: that the policy a
//! function RETURNED decides the method, that each of the three answers it can earn is given
//! synchronously with its own sentence and class, that the backend is observably un-run for
//! the refused ones, and that each answer is recorded — over BOTH the api's own published host
//! and the rewritten endpoint address a client that cannot resolve that host reaches it by.
//!
//! The rows differ along what the rule generalises over: WHAT the authorizer's function
//! returned for the presented token (an explicit `Deny`, an `Allow` naming the invoked
//! method), whether the identity source is carried at all, HOW the request addresses the stage
//! (the api's own `execute-api` host, and that endpoint carried in the path behind the reserved
//! `/_aws` namespace on the emulator's own endpoint), and whether the backend's log group
//! already holds another request's line when the absence is read — so an absence that only
//! holds because nothing has ever been written fails here.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling suites do.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region of both functions, of the
/// destination group and of the api's own invoke host. It is the Region the shared
/// stack-settling helper ([`wait_for_stack_status`]) reads a stack in.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the destination group's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that archives both inline handlers, the status it must reach, the role both
/// functions run as and its one inline document.
const STACK: &str = "xsvc-token-authorizer-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const EXECUTION_ROLE: &str = "xsvc-token-authorizer-runner";
const LOGGING_POLICY: &str = "logging";

/// The role API Gateway writes the account's logs as (`Account$cloudwatchRoleArn`).
const CLOUDWATCH_ROLE: &str = "xsvc-token-authorizer-apigw-cloudwatch";

/// The two functions: the one the authorizer names, and the one behind the method.
const AUTHORIZER_FUNCTION: &str = "xsvc-token-authorizer";
const BACKEND_FUNCTION: &str = "xsvc-token-authorizer-backend";

/// The stage the api is deployed to, and the resource path of its one gated method.
const STAGE: &str = "test";
const RESOURCE_PATH: &str = "secure";

/// The header the authorizer declares as its identity source, and the query parameter every
/// request carries its own marker in.
const TOKEN_HEADER: &str = "Authorization";
const MARKER_PARAMETER: &str = "marker";

/// The destination the stage's access log is pointed at, and the format it renders by.
const ACCESS_GROUP: &str = "/xsvc/apigw/token-authorizer-access";
const ACCESS_FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","path":"$context.path","#,
    r#""status":"$context.status"}"#
);

/// The line the backend prints, which is what a run of it leaves in its own log group.
const BACKEND_MARKER: &str = "BACKEND-INVOKED";

/// The sentences and classes a refusal carries: the explicit-deny body API Gateway answers a
/// call the returned policy DENIES, and the fixed `Unauthorized` body of a call that presented
/// nothing at the identity source.
const EXPLICIT_DENY: &str = "User is not authorized to access this resource with an explicit \
                             deny in an identity-based policy";
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The header API Gateway reports the id of the request it processed in, which the access-log
/// record's `$context.requestId` has to equal.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How long one invocation is given, as each function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the decision and the record, not how fast
/// an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work another service owes — a stack reaching `CREATE_COMPLETE`, a
/// function reaching `Active`, a delivered log record — and how long between two looks.
/// Generous on purpose: everything waited for happens in this process, so a loaded host does
/// not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The authorizer's function: it decides from the token it was handed, naming the method the
/// call addressed as the policy's resource, exactly as AWS's own Lambda-authorizer blueprint
/// does.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    token = event.get("authorizationToken") or ""
    effect = "Allow" if token == "allow-me" else "Deny"
    return {
        "principalId": "principal-" + token,
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Action": "execute-api:Invoke",
                    "Effect": effect,
                    "Resource": event["methodArn"],
                }
            ],
        },
    }
"#;

/// The function behind the method: it PRINTS the marker the request carried, so a run of it is
/// observable in its own log group, and answers the proxy envelope naming it.
const BACKEND_HANDLER: &str = r#"
import json


def handler(event, context):
    parameters = event.get("queryStringParameters") or {}
    marker = parameters.get("marker") or "no-marker"
    print("BACKEND-INVOKED " + marker)
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"ran": marker}),
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

    /// The sentence a refused answer carries: a gateway response words it under `message` or
    /// under `Message` depending on which response it is, so both spellings are read.
    fn message(&self) -> String {
        let body = self.json();
        for name in ["message", "Message"] {
            if let Some(message) = body.get(name).and_then(Value::as_str) {
                return message.to_string();
            }
        }
        self.text()
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine: both functions run on the Lambda this
/// router mounts, and every record is read from the CloudWatch Logs it mounts.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: the credential scope
    // is what routes an SDK request, and an invoke of the deployed stage carries none at all,
    // as a client of that URL sends none.
    router.set_request_authentication(false);
    router
}

/// The access key id this emulator publishes, which speaks for the account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// A SigV4 `Authorization` whose credential scope names `signing_name`. The signature itself
/// is never checked here.
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

/// One awsQuery call of `service` that has to be accepted, answered with its XML body.
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

/// One CloudWatch Logs call over its `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("logs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("authorization", &authorization("logs")),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
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

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// Create one role trusting `principal`, carrying the three log writes a delivery needs.
/// Answers the ARN IAM holds it under.
async fn create_role(router: &MultiServiceRouter, name: &str, principal: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let role_arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({name:?}) must answer an ARN"));
    let logging = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    accepted_query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", LOGGING_POLICY),
            ("PolicyDocument", &logging),
        ],
    )
    .await;
    role_arn
}

/// Deploy the stack declaring both functions: the inline sources are archived into deployment
/// packages by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for each, once it reports both `Active`.
async fn deploy_functions(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let declared = |name: &str, source: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role_arn,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": source },
            },
        })
    };
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Authorizer": declared(AUTHORIZER_FUNCTION, AUTHORIZER_HANDLER),
            "Backend": declared(BACKEND_FUNCTION, BACKEND_HANDLER),
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
    (
        active_function_arn(router, AUTHORIZER_FUNCTION).await,
        active_function_arn(router, BACKEND_FUNCTION).await,
    )
}

/// The ARN Lambda reports for `name`, once it reports it `Active` — a transition that service
/// owes, so this looks until it reports it rather than sleeping a span on its behalf.
async fn active_function_arn(router: &MultiServiceRouter, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    let configuration = loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
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
        "setup: Lambda must report {name:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Grant `apigateway.amazonaws.com` the invocation of `name`, scoped to the methods and the
/// authorizers of `api` — the `aws:SourceArn` such a grant is narrowed by, as a client scripts
/// it.
async fn grant(router: &MultiServiceRouter, name: &str, statement: &str, api: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        Some(json!({
            "StatementId": statement,
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/*"),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission({name:?}) must answer 201, got {}",
        granted.summary()
    );
}

/// The messages `FilterLogEvents` reports for `group`, or `None` when the account holds no such
/// group at all — which is what "nothing ran and nothing was created" looks like to a client.
async fn delivered(router: &MultiServiceRouter, group: &str) -> Option<Vec<String>> {
    let answered = logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    if answered.status != 200 {
        return None;
    }
    Some(
        answered.json()["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .map(|event| {
                        event["message"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Look in `group` until one message satisfying `wanted` is held, and answer it; `None` once
/// the deadline passes with none. The wait is on the CONDITION — a delivery another service
/// makes — so it polls until it holds rather than sleeping a span.
async fn awaited_message(
    router: &MultiServiceRouter,
    group: &str,
    wanted: impl Fn(&str) -> bool,
) -> Option<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(messages) = delivered(router, group).await {
            if let Some(found) = messages.into_iter().find(|message| wanted(message)) {
                return Some(found);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// An address of the emulator's own endpoint, which names no service at all: what a client
/// that cannot resolve the api's own host sends to.
const LOCAL_ENDPOINT: &str = "127.0.0.1:9297";

/// HOW a row addresses the deployed stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    /// `{api-id}.execute-api.{region}.amazonaws.com/{stage}/{resource}` — the endpoint
    /// `RestApi$disableExecuteApiEndpoint` documents.
    OwnHost,
    /// That endpoint carried in the PATH behind the reserved `/_aws` namespace, on the
    /// emulator's own endpoint: the address a client that resolves the host above nowhere
    /// rewrites it to.
    RewrittenEndpoint,
}

/// One row: what the request presents at the identity source, how it addresses the stage, and
/// what the stage owes it.
struct Row {
    case: &'static str,
    /// The value at the authorizer's identity source, or `None` for a request carrying none.
    token: Option<&'static str>,
    addressed: Addressed,
    status: u16,
    /// The sentence a refused answer carries, or `None` for the row the integration serves.
    message: Option<&'static str>,
    /// The class a refusal names in `x-amzn-errortype`, where the rule fixes one.
    error_type: Option<&'static str>,
    /// Whether the method's integration — the backend function — runs at all.
    backend_runs: bool,
}

/// The table the rule is asserted over, in order: the first `Deny` is answered before anything
/// has ever run, and the second after the backend's group holds another request's line, so an
/// absence that only holds because the group is empty fails on that row.
const ROWS: &[Row] = &[
    Row {
        case: "a token whose policy explicitly denies the invoked method",
        token: Some("deny-me"),
        addressed: Addressed::OwnHost,
        status: 403,
        message: Some(EXPLICIT_DENY),
        error_type: Some(ACCESS_DENIED_EXCEPTION),
        backend_runs: false,
    },
    Row {
        case: "a token whose policy allows the invoked method",
        token: Some("allow-me"),
        addressed: Addressed::OwnHost,
        status: 200,
        message: None,
        error_type: None,
        backend_runs: true,
    },
    Row {
        case: "that same denied token at the rewritten endpoint, once the backend's group \
               holds another run",
        token: Some("deny-me"),
        addressed: Addressed::RewrittenEndpoint,
        status: 403,
        message: Some(EXPLICIT_DENY),
        error_type: Some(ACCESS_DENIED_EXCEPTION),
        backend_runs: false,
    },
    Row {
        case: "the allowing token at that same rewritten endpoint",
        token: Some("allow-me"),
        addressed: Addressed::RewrittenEndpoint,
        status: 200,
        message: None,
        error_type: None,
        backend_runs: true,
    },
    Row {
        case: "a request carrying nothing at the identity source",
        token: None,
        addressed: Addressed::OwnHost,
        status: 401,
        message: Some(UNAUTHORIZED_MESSAGE),
        error_type: None,
        backend_runs: false,
    },
    Row {
        case: "a request carrying nothing at the identity source, at the rewritten endpoint",
        token: None,
        addressed: Addressed::RewrittenEndpoint,
        status: 401,
        message: Some(UNAUTHORIZED_MESSAGE),
        error_type: None,
        backend_runs: false,
    },
];

/// RULE: a `CUSTOM`-gated method of a deployed stage answers every request synchronously with
/// what its authorizer's function decided, never performs the integration for a refused one,
/// and leaves one access-log record per answered request in the destination its
/// `accessLogSettings` name.
#[test]
fn a_token_authorizer_decides_the_stage_and_the_stage_records_what_it_answered() {
    std::thread::Builder::new()
        .name("apigateway-token-authorizer".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on (`core/router/src/main.rs`),
            // because one invoke here drives two nested in-process calls on it — the
            // authorizer's invocation and the integration's — and each of those drives the log
            // deliveries of its own run.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                // The IO and TIME drivers a service's background work needs.
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

    // 1. The roles, the two functions, and the destination group the stage is pointed at.
    let execution_role = create_role(&router, EXECUTION_ROLE, "lambda.amazonaws.com").await;
    let cloudwatch_role = create_role(&router, CLOUDWATCH_ROLE, "apigateway.amazonaws.com").await;
    let (authorizer_arn, backend_arn) = deploy_functions(&router, &execution_role).await;
    let created = logs(
        &router,
        "CreateLogGroup",
        &json!({ "logGroupName": ACCESS_GROUP }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateLogGroup({ACCESS_GROUP:?}) must answer 200, got {}",
        created.summary()
    );

    // 2. The api, exactly as a CLI deployment builds one: the resource, the TOKEN authorizer,
    //    the gated method, the proxy integration, the grants and the deployment that mints the
    //    stage.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "token-authorizer-gated-api" })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &api_record, "id");
    let root = as_str("CreateRestApi", &api_record, "rootResourceId");
    let resource = apigw(
        &router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource_id = as_str("CreateResource", &resource, "id");
    let authorizer = apigw(
        &router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "token-gate",
            "type": "TOKEN",
            "identitySource": format!("method.request.header.{TOKEN_HEADER}"),
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{authorizer_arn}/\
                 invocations"
            ),
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
    grant(&router, AUTHORIZER_FUNCTION, "apigw-authorizer", &api).await;
    grant(&router, BACKEND_FUNCTION, "apigw-backend", &api).await;
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    apigw(
        &router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer_id })),
        201,
    )
    .await;
    apigw(
        &router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{backend_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    // 3. The account's CloudWatch role, which is what API Gateway writes the account's logs
    //    as, and the stage's own access logging.
    apigw(
        &router,
        "UpdateAccount",
        "PATCH",
        "/account",
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/cloudwatchRoleArn", "value": cloudwatch_role },
            ],
        })),
        200,
    )
    .await;
    apigw(
        &router,
        "UpdateStage accessLogSettings",
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": "/accessLogSettings/destinationArn",
                    "value": format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{ACCESS_GROUP}"),
                },
                { "op": "replace", "path": "/accessLogSettings/format", "value": ACCESS_FORMAT },
            ],
        })),
        200,
    )
    .await;

    // 4. The rows: each presents its own value at the identity source and names itself in the
    //    marker the backend would print.
    let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let backend_group = format!("/aws/lambda/{BACKEND_FUNCTION}");
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = format!("row-{index}");
        let headers: Vec<(&str, &str)> = match row.token {
            Some(token) => vec![(TOKEN_HEADER, token)],
            None => Vec::new(),
        };
        let invoke_path = format!("/{STAGE}/{RESOURCE_PATH}?{MARKER_PARAMETER}={marker}");
        let (host, path) = match row.addressed {
            Addressed::OwnHost => (invoke_host.clone(), invoke_path.clone()),
            // The endpoint the client was handed, carried in the path: the reserved namespace,
            // then that endpoint's own HOST, then the invoke path whole.
            Addressed::RewrittenEndpoint => (
                LOCAL_ENDPOINT.to_string(),
                format!("/_aws/{invoke_host}{invoke_path}"),
            ),
        };
        let answered = dispatch(&router, "GET", &host, &path, &headers, Bytes::new()).await;

        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} from the stage, got {}",
            row.status,
            answered.summary()
        );
        match row.message {
            Some(sentence) => assert_eq!(
                answered.message(),
                sentence,
                "case {case:?}: expected the refusal {sentence:?}, got {}",
                answered.summary()
            ),
            None => assert_eq!(
                answered.json(),
                json!({ "ran": marker }),
                "case {case:?}: expected the integration's own answer for {marker:?}, got {}",
                answered.summary()
            ),
        }
        if let Some(class) = row.error_type {
            assert_eq!(
                answered.header(ERROR_TYPE_HEADER).as_deref(),
                Some(class),
                "case {case:?}: expected {ERROR_TYPE_HEADER:?} to name {class:?}, got {:?}",
                answered.header(ERROR_TYPE_HEADER)
            );
        }

        // The stage logged the request it just answered: one record carrying this request's
        // own id, the status it was answered with and the stage-qualified path addressed.
        let request_id = answered
            .header(REQUEST_ID_HEADER)
            .unwrap_or_else(|| panic!("case {case:?}: the answer reported no request id"));
        let awaited = awaited_message(&router, ACCESS_GROUP, |message| {
            message.contains(&request_id)
        })
        .await;
        let held = delivered(&router, ACCESS_GROUP).await;
        let record = awaited.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an access-log record for {request_id:?} in \
                 {ACCESS_GROUP:?}, got {held:?}"
            )
        });
        assert_eq!(
            serde_json::from_str::<Value>(&record).unwrap_or(Value::Null),
            json!({
                "requestId": request_id,
                "path": format!("/{STAGE}/{RESOURCE_PATH}"),
                "status": row.status.to_string(),
            }),
            "case {case:?}: expected the stage's own record of this request, got {record:?}"
        );
        // ONE record per answered request, which is what the stage owes: a second delivery of
        // the same request fails here.
        let for_this_request = delivered(&router, ACCESS_GROUP)
            .await
            .unwrap_or_default()
            .iter()
            .filter(|message| message.contains(&request_id))
            .count();
        assert_eq!(
            for_this_request, 1,
            "case {case:?}: expected exactly one access-log record for {request_id:?}, got \
             {for_this_request}"
        );

        // And what the BACKEND ran: the line this request's marker names, present only where
        // the authorizer admitted the call. The absence is read once the record above arrived,
        // so the gateway has finished answering the request by then.
        let printed = format!("{BACKEND_MARKER} {marker}");
        let ran = if row.backend_runs {
            awaited_message(&router, &backend_group, |message| message.contains(&printed))
                .await
                .is_some()
        } else {
            delivered(&router, &backend_group)
                .await
                .unwrap_or_default()
                .iter()
                .any(|message| message.contains(&printed))
        };
        assert_eq!(
            ran,
            row.backend_runs,
            "case {case:?}: expected the backend to have run={}, got the group \
             {backend_group:?} holding {:?}",
            row.backend_runs,
            delivered(&router, &backend_group).await
        );
    }
}
