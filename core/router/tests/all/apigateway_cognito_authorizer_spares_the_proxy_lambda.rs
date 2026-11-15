// SPDX-License-Identifier: CC0-1.0

//! What a client of a DEPLOYED REST stage is answered when the method behind it is gated by a
//! `COGNITO_USER_POOLS` authorizer and fronts an `AWS_PROXY` integration over a Lambda
//! function — and whether that function RAN.
//!
//! RULE: **a deployed method whose `authorizationType` is `COGNITO_USER_POOLS` decides every
//! request at the edge, before its integration, and answers it synchronously: a request whose
//! value at the authorizer's `Authorizer$identitySource` is not a token one of the user pools
//! that authorizer's `providerARNs` name signed authorized nobody, so it is answered `401`
//! `{"message":"Unauthorized"}` with the `UnauthorizedException` class named in
//! `x-amzn-errortype` and the integration is NOT dispatched; a request presenting a token such
//! a pool signed reaches the integration, which is invoked exactly once and whose own proxy
//! envelope is what the client is served.**
//!
//! `Method$authorizationType` gives `COGNITO_USER_POOLS` "for using a Cognito user pool",
//! `Authorizer$providerARNs` is "a list of the Amazon Cognito user pool ARNs for the
//! `COGNITO_USER_POOLS` authorizer" and `Authorizer$identitySource` "specifies the request
//! header mapping expression for the custom header holding the authorization token"; the
//! `401` carries the fixed sentence of a request that authenticated nobody, a class API
//! Gateway models as `UnauthorizedException` with its own `httpError` 401.
//!
//! Four crates have to agree for that to hold — Amazon Cognito user pools mint the tokens and
//! publish the key set they verify against, CloudFormation archives the inline handler into a
//! package this emulator can run, Lambda runs it, Amazon SQS holds the side channel the run
//! reports to, and API Gateway resolves the deployed stage and takes the decision — so the
//! suite lives here rather than in any one of them. The sibling
//! `apigateway_cognito_user_pools_authorizer` suite pins WHICH refusal each class of token
//! earns behind a `MOCK` integration; what only this suite reaches is the other half of the
//! rule: that a refused leg leaves the backing function UNINVOKED while an admitted one
//! invokes it exactly once, and that every leg is answered at all rather than left hanging.
//!
//! That the integration ran is asserted where nothing of API Gateway can fake it: the handler
//! reports every invocation to a queue through its own SDK before returning, so a leg the gate
//! refused leaves nothing in that queue and an admitted leg leaves exactly one message.
//!
//! The rows differ along what the rule generalises over: WHETHER the identity source is
//! carried at all (absent, empty), WHAT it carries (a value that is no token, the same value
//! behind the RFC 6750 bearer scheme, a token minted by a pool the authorizer does not name,
//! the identity token and the access token of the pool it does name), and so WHETHER the
//! request reaches the integration.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling Cognito suite does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cognito_group_role_mapping::{claims_of, GOOD_PASSWORD};
use crate::query_wire::{form_encode, opt_element};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region of each pool's issuer, of
/// the queue and of the api's own invoke host. It is the Region the shared stack-settling
/// helper ([`wait_for_stack_status`]) reads a stack in, so the stack this suite deploys is the
/// one that helper waits for.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the queue's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The side-channel queue the handler reports every invocation to, the role the function runs
/// as and its two inline documents, the stack that archives the handler, and the function.
const QUEUE: &str = "xsvc-cognito-proxy-side-channel";
const EXECUTION_ROLE: &str = "xsvc-cognito-proxy-runner";
const LOGGING_POLICY: &str = "logging";
const REPORT_POLICY: &str = "report-invocations";
const STACK: &str = "xsvc-cognito-proxy-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const FUNCTION: &str = "xsvc-cognito-proxy-backend";

/// The stage the api is deployed to, and the resource path of its one gated method.
const STAGE: &str = "prod";
const RESOURCE_PATH: &str = "secure";

/// The header the authorizer's identity source names, and the query parameter every request
/// carries its own marker in — which is what the handler reports, so one message names the leg
/// that produced it.
const TOKEN_HEADER: &str = "Authorization";
const MARKER_PARAMETER: &str = "marker";

/// What the gate owes a request that authorized nobody: the status, the body's one member and
/// its fixed sentence, and the class the endpoint names it in.
const UNAUTHORIZED: u16 = 401;
const MESSAGE_MEMBER: &str = "message";
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const UNAUTHORIZED_EXCEPTION: &str = "UnauthorizedException";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the gate's decision and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work the clock owes — a stack reaching `CREATE_COMPLETE`, a function
/// reaching `Active`, a message the queue is owed — and how long between two looks. Generous
/// on purpose: everything waited for happens in this process, so a loaded host does not decide
/// the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the gated method fronts. It reports the invocation to the side channel FIRST,
/// so a leg answered without the handler having been reached leaves the queue empty, and then
/// answers a proxy envelope naming the claims the authorizer published for the token that
/// admitted the request.
const REPORTING_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    parameters = event.get("queryStringParameters") or {}
    marker = parameters.get("marker") or "no-marker"
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["QUEUE_URL"], MessageBody=marker
    )
    context_members = event.get("requestContext") or {}
    authorizer = context_members.get("authorizer") or {}
    claims = authorizer.get("claims") or {}
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {
                "marker": marker,
                "sub": claims.get("sub", ""),
                "use": claims.get("token_use", ""),
            }
        ),
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

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the queue it reports to is the queue this suite reads, and
/// the pools whose key sets the gate reads are the pools this suite mints.
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

/// One awsQuery call of `service`, answered with its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
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
    .await
}

/// One awsQuery call that has to be accepted, answered with its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let answered = query(router, service, version, action, members).await;
    assert_eq!(
        answered.status,
        200,
        "setup: {service} {action} must answer 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One awsJson1_0 call to Amazon SQS, answered with its status and its parsed body.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("sqs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("sqs")),
            ("x-amz-target", &format!("AmazonSQS.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One awsJson1_1 call to the Amazon Cognito user pools service, which must succeed.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answered = dispatch(
        router,
        "POST",
        &format!("cognito-idp.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            (
                "x-amz-target",
                &format!("AWSCognitoIdentityProviderService.{operation}"),
            ),
            ("authorization", &authorization("cognito-idp")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: cognito-idp {operation} must answer 200, got {}",
        answered.summary()
    );
    answered.json()
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

/// One claim of a minted token, as a string.
///
/// The compact-JWS reading itself is the sibling suite's ([`claims_of`]), which is the one
/// reader of a minted token's claim set in this target.
fn claim(token: &str, name: &str) -> String {
    let claims = claims_of(token);
    claims
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a minted token must carry a string {name:?} claim, got {claims}"))
        .to_string()
}

/// What one pool's sign-in produced: the pool's ARN and the two tokens of the sign-in.
struct Pool {
    arn: String,
    id_token: String,
    access_token: String,
}

/// Mint one pool, one app client and one confirmed user, and sign that user in.
async fn pool_with_signed_in_user(
    router: &MultiServiceRouter,
    pool_name: &str,
    username: &str,
) -> Pool {
    let pool = cognito(router, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");
    let arn = as_str("CreateUserPool", &pool["UserPool"], "Arn");
    let client = cognito(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": pool_id,
            "ClientName": format!("{pool_name}-client"),
            // The flow the sign-in below uses, plus the refresh flow AWS requires beside any
            // other `ALLOW_` flow.
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
        }),
    )
    .await;
    let client_id = as_str("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");
    cognito(
        router,
        "AdminCreateUser",
        &json!({ "UserPoolId": pool_id, "Username": username }),
    )
    .await;
    cognito(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true
        }),
    )
    .await;
    let signed_in = cognito(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD }
        }),
    )
    .await;
    let result = &signed_in["AuthenticationResult"];
    Pool {
        arn,
        id_token: as_str("AdminInitiateAuth", result, "IdToken"),
        access_token: as_str("AdminInitiateAuth", result, "AccessToken"),
    }
}

/// The ARN of the side-channel queue, which the handler's one grant names.
fn queue_arn() -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{QUEUE}")
}

/// Create the side-channel queue and answer the url Amazon SQS published for it.
async fn create_queue(router: &MultiServiceRouter) -> String {
    let created = sqs(router, "CreateQueue", &json!({ "QueueName": QUEUE })).await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateQueue({QUEUE:?}) must answer 200, got {}",
        created.summary()
    );
    as_str("CreateQueue", &created.json(), "QueueUrl")
}

/// Create the execution role the function runs as, carrying the log grants every execution role
/// needs and the one send grant the handler's own call asks for. Answers the role's ARN.
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
    let role_arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({EXECUTION_ROLE:?}) must answer an ARN"));
    let logging = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    let reporting = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "sqs:SendMessage",
            "Resource": queue_arn(),
        }],
    })
    .to_string();
    for (policy, document) in [(LOGGING_POLICY, &logging), (REPORT_POLICY, &reporting)] {
        accepted_query(
            router,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", EXECUTION_ROLE),
                ("PolicyName", policy),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }
    role_arn
}

/// Deploy the stack declaring the function: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deploy_function(
    router: &MultiServiceRouter,
    role_arn: &str,
    queue_url: &str,
) -> String {
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
                    "Environment": { "Variables": { "QUEUE_URL": queue_url } },
                    "Code": { "ZipFile": REPORTING_HANDLER },
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
    // The invoke the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
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

/// Every marker the side channel holds, deleted as it is read, polled until `wanted` is among
/// them or the budget runs out. The leg that produces `wanted` is the LAST one driven, and each
/// leg's response was awaited before the next request was sent, so a message any earlier leg
/// produced is already in the queue by the time this one is.
async fn reported(router: &MultiServiceRouter, queue_url: &str, wanted: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let deadline = Instant::now() + BUDGET;
    loop {
        let received = sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        assert_eq!(
            received.status,
            200,
            "ReceiveMessage must answer 200, got {}",
            received.summary()
        );
        let body = received.json();
        let batch = body["Messages"].as_array().cloned().unwrap_or_default();
        for message in &batch {
            seen.push(message["Body"].as_str().unwrap_or_default().to_string());
            let handle = message["ReceiptHandle"].as_str().unwrap_or_default();
            let deleted = sqs(
                router,
                "DeleteMessage",
                &json!({ "QueueUrl": queue_url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                deleted.status,
                200,
                "DeleteMessage must answer 200, got {}",
                deleted.summary()
            );
        }
        if seen.iter().any(|marker| marker == wanted) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What the gate owes one leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owed {
    /// The authorizer does not admit what the request presented, so it authorized nobody: the
    /// `401` sentence, and the integration never dispatched.
    Unauthorized,
    /// The authorizer admitted the request: the integration ran and its envelope is served,
    /// naming the claims of a token of this `token_use`.
    Served { token_use: &'static str },
}

/// WHICH value a leg presents at the authorizer's identity source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// No `Authorization` header at all — the recorded unauthorized-negative leg.
    Nothing,
    /// The header, carrying nothing.
    Empty,
    /// A value that is no token at all.
    NotAToken,
    /// That same value behind the RFC 6750 bearer scheme.
    BearerNotAToken,
    /// The identity token of a pool the authorizer does NOT name.
    ForeignIdToken,
    /// The access token of that same foreign pool.
    ForeignAccessToken,
    /// The identity token of the pool the authorizer names.
    HomeIdToken,
    /// The access token of that same pool.
    HomeAccessToken,
}

/// One request to the deployed stage.
struct Row {
    case: &'static str,
    presented: Presented,
    owed: Owed,
}

/// The rows, in order. Every refused row runs BEFORE the admitted ones, so the queue's contents
/// at the end name exactly the legs that reached the integration.
const ROWS: &[Row] = &[
    Row {
        case: "no Authorization header at all",
        presented: Presented::Nothing,
        owed: Owed::Unauthorized,
    },
    Row {
        case: "an Authorization header carrying nothing",
        presented: Presented::Empty,
        owed: Owed::Unauthorized,
    },
    Row {
        case: "a value that is no token at all",
        presented: Presented::NotAToken,
        owed: Owed::Unauthorized,
    },
    Row {
        case: "that same value behind the bearer scheme",
        presented: Presented::BearerNotAToken,
        owed: Owed::Unauthorized,
    },
    Row {
        case: "an identity token of a pool this authorizer does not name",
        presented: Presented::ForeignIdToken,
        owed: Owed::Unauthorized,
    },
    Row {
        case: "an access token of that same foreign pool",
        presented: Presented::ForeignAccessToken,
        owed: Owed::Unauthorized,
    },
    Row {
        case: "the identity token of the pool this authorizer names",
        presented: Presented::HomeIdToken,
        owed: Owed::Served { token_use: "id" },
    },
    Row {
        case: "the access token of that same pool, the method declaring no scopes",
        presented: Presented::HomeAccessToken,
        owed: Owed::Served {
            token_use: "access",
        },
    },
];

/// RULE: a `COGNITO_USER_POOLS`-gated method of a deployed stage answers every request
/// synchronously at the edge — `401` `{"message":"Unauthorized"}` with the integration left
/// undispatched for a value no named pool signed, and the integration's own envelope, invoked
/// exactly once, for a token one of them did.
#[test]
fn a_cognito_gated_proxy_method_refuses_at_the_edge_without_invoking_its_function() {
    std::thread::Builder::new()
        .name("apigateway-cognito-proxy-gate".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(suite());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}

async fn suite() {
    let router = emulator();

    // 1. The side channel, the role, and the function that reports to it.
    let queue_url = create_queue(&router).await;
    let role_arn = create_execution_role(&router).await;
    let function_arn = deploy_function(&router, &role_arn, &queue_url).await;
    let granted = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission must answer 201, got {}",
        granted.summary()
    );

    // 2. The pool whose tokens the method accepts, and a second pool whose tokens are from an
    //    issuer the authorizer does not name.
    let home = pool_with_signed_in_user(&router, "proxy-gate-pool", "gate-user").await;
    let foreign = pool_with_signed_in_user(&router, "proxy-gate-other-pool", "other-user").await;
    let home_sub = claim(&home.id_token, "sub");
    assert_eq!(
        home_sub,
        claim(&home.access_token, "sub"),
        "both tokens of one sign-in must name the same subject"
    );

    // 3. The api, exactly as a CLI deployment builds one: the resource, the user-pool
    //    authorizer, the gated method, the proxy integration and the deployment that mints the
    //    stage.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cognito-proxy-gated-api" })),
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
    let identity_source = format!("method.request.header.{TOKEN_HEADER}");
    let authorizer = apigw(
        &router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "pool-gate",
            "type": "COGNITO_USER_POOLS",
            "providerARNs": [home.arn],
            "identitySource": identity_source,
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    apigw(
        &router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({
            "authorizationType": "COGNITO_USER_POOLS",
            "authorizerId": authorizer_id,
        })),
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
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
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

    // The gate's decision is taken on PERSISTED records, which is what a read of them reports.
    let read_authorizer = apigw(
        &router,
        "GetAuthorizer",
        "GET",
        &format!("/restapis/{api}/authorizers/{authorizer_id}"),
        None,
        200,
    )
    .await;
    assert_eq!(
        read_authorizer["type"].as_str(),
        Some("COGNITO_USER_POOLS"),
        "the stored authorizer must be a user-pool one, got {read_authorizer}"
    );
    assert_eq!(
        read_authorizer["providerARNs"],
        json!([home.arn]),
        "the stored authorizer must name the pool it was created with, got {read_authorizer}"
    );
    assert_eq!(
        read_authorizer["identitySource"].as_str(),
        Some(identity_source.as_str()),
        "the stored authorizer must keep its identity source, got {read_authorizer}"
    );
    let read_method = apigw(&router, "GetMethod", "GET", &method, None, 200).await;
    assert_eq!(
        read_method["authorizationType"].as_str(),
        Some("COGNITO_USER_POOLS"),
        "the stored method must be gated by the user pool, got {read_method}"
    );
    assert_eq!(
        read_method["authorizerId"].as_str(),
        Some(authorizer_id.as_str()),
        "the stored method must name the authorizer created for it, got {read_method}"
    );

    // 4. The legs, in order: each one presents its own value at the identity source and names
    //    itself in the marker the handler reports.
    let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let bearer_for = |token: &str| format!("Bearer {token}");
    let mut expected_markers: Vec<String> = Vec::new();
    let mut last_admitted = String::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = format!("leg-{index}");
        let presented: Option<String> = match row.presented {
            Presented::Nothing => None,
            Presented::Empty => Some(String::new()),
            Presented::NotAToken => Some("not-a-real-cognito-token".to_string()),
            Presented::BearerNotAToken => Some(bearer_for("not-a-real-cognito-token")),
            Presented::ForeignIdToken => Some(foreign.id_token.clone()),
            Presented::ForeignAccessToken => Some(foreign.access_token.clone()),
            Presented::HomeIdToken => Some(home.id_token.clone()),
            Presented::HomeAccessToken => Some(bearer_for(&home.access_token)),
        };
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(value) = presented.as_deref() {
            headers.push((TOKEN_HEADER, value));
        }
        let answered = dispatch(
            &router,
            "GET",
            &invoke_host,
            &format!("/{STAGE}/{RESOURCE_PATH}?{MARKER_PARAMETER}={marker}"),
            &headers,
            Bytes::new(),
        )
        .await;
        match row.owed {
            Owed::Unauthorized => {
                assert_eq!(
                    answered.status, UNAUTHORIZED,
                    "case {case:?}: expected HTTP {UNAUTHORIZED} from the gate, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.json(),
                    json!({ MESSAGE_MEMBER: UNAUTHORIZED_MESSAGE }),
                    "case {case:?}: expected the fixed refusal body, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.header(ERROR_TYPE_HEADER).as_deref(),
                    Some(UNAUTHORIZED_EXCEPTION),
                    "case {case:?}: expected {ERROR_TYPE_HEADER:?} to name \
                     {UNAUTHORIZED_EXCEPTION:?}, got {:?}",
                    answered.header(ERROR_TYPE_HEADER)
                );
            }
            Owed::Served { token_use } => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: expected the integration's own 200, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.json(),
                    json!({ "marker": marker, "sub": home_sub, "use": token_use }),
                    "case {case:?}: expected the handler's envelope naming the admitted \
                     token's claims, got {}",
                    answered.summary()
                );
                expected_markers.push(marker.clone());
                last_admitted = marker;
            }
        }
    }

    // 5. What the integration actually RAN: one message per admitted leg and not one more, so a
    //    refused leg that reached the function fails here even though it was answered the
    //    refusal.
    assert!(
        !last_admitted.is_empty(),
        "the table must drive at least one admitted leg"
    );
    let mut seen = reported(&router, &queue_url, &last_admitted).await;
    seen.sort();
    let mut wanted = expected_markers;
    wanted.sort();
    assert_eq!(
        seen, wanted,
        "expected the function to have been invoked exactly once per admitted leg \
         ({wanted:?}), got {seen:?}"
    );
}
