// SPDX-License-Identifier: CC0-1.0

//! What a `COGNITO_USER_POOLS`-gated method of a deployed REST stage answers, and what its
//! backend LEFT BEHIND in its own CloudWatch Logs group — the evidence a client of a deployed
//! api reads to tell "the gate refused it" from "the gate let it through", asserted on the
//! emulator as it is ASSEMBLED.
//!
//! RULE: **a deployed method whose `authorizationType` is `COGNITO_USER_POOLS` decides every
//! request at the edge, BEFORE its integration, and answers it synchronously: a value at the
//! authorizer's `Authorizer$identitySource` that is not a token signed by one of the user pools
//! the authorizer's `providerARNs` name authorized nobody, so the request is answered `401`
//! `{"message":"Unauthorized"}` with the `UnauthorizedException` class named in
//! `x-amzn-errortype` and the method's `AWS_PROXY` function is never run — which is observable
//! as NOTHING of a run of it reaching the function's own log group; a request presenting a token
//! such a pool signed reaches the integration, which is invoked exactly once, whose envelope is
//! what the client is served and whose whole run — the line the handler printed inside the
//! runtime's framing — is in that group afterwards.**
//!
//! `Method$authorizationType` gives `COGNITO_USER_POOLS` "for using a Cognito user pool",
//! `Authorizer$providerARNs` is "a list of the Amazon Cognito user pool ARNs for the
//! `COGNITO_USER_POOLS` authorizer" and `Authorizer$identitySource` "specifies the request
//! header mapping expression for the custom header holding the authorization token"; "By
//! default, Lambda functions send logs to a default log group named `/aws/lambda/<function
//! name>`" (`LoggingConfig$LogGroup`) is what makes a run of the backend — or its absence —
//! readable through `FilterLogEvents`, which is where a client looks to find out whether the
//! backend ran at all.
//!
//! Six crates have to agree for that to hold — Amazon Cognito user pools mint the token and
//! publish the key set it is verified against, IAM holds the execution role the log writes are
//! judged against, CloudFormation archives the inline handler into a package this emulator can
//! run, Lambda runs it and delivers the run's log, CloudWatch Logs holds the group that log is
//! read from, and API Gateway resolves the deployed stage and takes the decision — so the suite
//! lives here rather than in any one of them.
//!
//! What the neighbouring assembled-router suites already pin, and this one does not repeat:
//! [`crate::apigateway_cognito_user_pools_authorizer`] pins WHICH refusal each class of token
//! earns behind a `MOCK` integration,
//! [`crate::apigateway_cognito_authorizer_spares_the_proxy_lambda`] pins the
//! refused-leg-spares-the-backend shape as the handler's OWN SDK call reports it, and
//! [`crate::apigateway_token_authorizer_deny_and_access_log`] pins the same for a `TOKEN`
//! authorizer's policy. What only this suite reaches is the pool-gated leg read where a client
//! actually reads it — the function's own log group — with an execution role carrying AWS's own
//! managed execution-role policy rather than an inline document, and with the invocation
//! admitted by the WILDCARD `aws:SourceArn` a deployment of a whole api scopes its grant by.
//!
//! The rows differ along what the rule generalises over: WHAT the request presents at the
//! identity source (nothing at all, the header carrying nothing, a value that is no token behind
//! the RFC 6750 bearer scheme, the identity token of the named pool, the access token of that
//! same sign-in), and WHETHER the backend's group already holds another request's run when the
//! absence is read — so an absence that only holds because nothing has ever been written fails
//! here.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling suites do.

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

/// The Region every request here is signed for, and so the Region of the pool's issuer, of the
/// function and of the api's own invoke host. It is the Region the shared stack-settling helper
/// ([`wait_for_stack_status`]) reads a stack in, so the stack this suite deploys is the one that
/// helper waits for.
const REGION: &str = "us-east-1";

/// The stack that archives the inline handler, the status it must reach, the role the function
/// runs as, and the function itself.
const STACK: &str = "xsvc-cognito-run-log-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const EXECUTION_ROLE: &str = "xsvc-cognito-run-log-runner";
const BACKEND_FUNCTION: &str = "xsvc-cognito-run-log-backend";

/// AWS's own managed execution-role policy for a function that writes nothing but its log —
/// `logs:CreateLogGroup`, `logs:CreateLogStream` and `logs:PutLogEvents` — ATTACHED rather than
/// written out here, because that is what a deployment of a Lambda-backed api grants its role.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The stage the api is deployed to, and the resource path of its one gated method.
const STAGE: &str = "test";
const RESOURCE_PATH: &str = "secure";

/// The header the authorizer declares as its identity source, and the query parameter every
/// request carries its own marker in — which is what the handler prints, so one line of the
/// group names the leg that produced it.
const TOKEN_HEADER: &str = "Authorization";
const MARKER_PARAMETER: &str = "marker";

/// The pool whose tokens the method accepts, the app client the sign-in is made through, and the
/// user who signs in. The password is the shared fixture one ([`GOOD_PASSWORD`]), which satisfies
/// a pool's default password policy.
const POOL_NAME: &str = "xsvc-cognito-run-log-pool";
const USERNAME: &str = "run-log-user";

/// What the gate owes a request that authorized nobody: the status, the body's one member and
/// its fixed sentence, and the class the endpoint names it in.
const UNAUTHORIZED: u16 = 401;
const MESSAGE_MEMBER: &str = "message";
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const UNAUTHORIZED_EXCEPTION: &str = "UnauthorizedException";

/// What a read of a log group that does not exist is refused with: "Lists log events from the
/// specified log group" — there is nothing to list until that group exists — which CloudWatch
/// Logs models as `ResourceNotFoundException` with its own `httpError` 400. This is what the
/// ABSENCE of a function's group looks like to a client, as opposed to an empty page.
const GROUP_ABSENT_STATUS: u16 = 400;
const GROUP_ABSENT_EXCEPTION: &str = "ResourceNotFoundException";

/// The line the backend prints, which is what a run of it leaves in its own log group, and the
/// record the runtime opens every run's log with.
const BACKEND_MARKER: &str = "BACKEND-INVOKED";
const START_RECORD: &str = "START RequestId: ";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the gate's decision and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work the clock owes — a stack reaching `CREATE_COMPLETE`, a function
/// reaching `Active`, a run's log reaching its group — and how long between two looks. Generous
/// on purpose: everything waited for happens in this process, so a loaded host does not decide
/// the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the gated method fronts. It PRINTS the marker the request carried — so a run of
/// it leaves that line in its own log group — and answers a proxy envelope naming the marker and
/// the signed-in subject the gate published, so a run is observable to the client too.
const BACKEND_HANDLER: &str = r#"
import json


def handler(event, context):
    marker = (event.get("queryStringParameters") or {}).get("marker") or "no-marker"
    print("BACKEND-INVOKED " + marker)
    authorizer = (event.get("requestContext") or {}).get("authorizer") or {}
    claims = authorizer.get("claims") or {}
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"ran": marker, "sub": claims.get("sub", "")}),
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

/// The emulator as it is ASSEMBLED, on one engine: the pool whose key set the gate reads is the
/// pool this suite mints, and the group the run's log lands in is the group this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: the credential scope is
    // what routes an SDK request, and an invoke of the deployed stage carries none at all, as a
    // client of that URL sends none.
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

/// A required non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// What one pool's sign-in produced: the pool's ARN, the two tokens of that sign-in and the
/// subject they name.
struct SignedIn {
    pool_arn: String,
    id_token: String,
    access_token: String,
    subject: String,
}

/// Mint one pool, one app client and one confirmed user, and sign that user in — so every token
/// this suite presents is one the pool ITSELF minted and signed.
async fn signed_in_user(router: &MultiServiceRouter) -> SignedIn {
    let pool = cognito(router, "CreateUserPool", &json!({ "PoolName": POOL_NAME })).await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");
    let pool_arn = as_str("CreateUserPool", &pool["UserPool"], "Arn");
    let client = cognito(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": pool_id,
            "ClientName": format!("{POOL_NAME}-client"),
            // The flow the sign-in below uses, plus the refresh flow AWS requires beside any
            // other `ALLOW_` flow.
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = as_str("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");
    cognito(
        router,
        "AdminCreateUser",
        &json!({ "UserPoolId": pool_id, "Username": USERNAME }),
    )
    .await;
    cognito(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let authenticated = cognito(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    let result = &authenticated["AuthenticationResult"];
    let id_token = as_str("AdminInitiateAuth", result, "IdToken");
    let access_token = as_str("AdminInitiateAuth", result, "AccessToken");
    let subject = claim_of(&id_token, "sub");
    assert_eq!(
        subject,
        claim_of(&access_token, "sub"),
        "both tokens of one sign-in must name the same subject"
    );
    SignedIn {
        pool_arn,
        id_token,
        access_token,
        subject,
    }
}

/// One claim of a minted token, as text.
///
/// The compact-JWS reading itself is the shared one ([`claims_of`]), which is this target's one
/// reader of a minted token's claim set.
fn claim_of(token: &str, name: &str) -> String {
    as_str("the minted token's claims", &claims_of(token), name)
}

/// Create the execution role the function runs as, carrying AWS's own managed execution-role
/// policy — the three log writes a run makes — and answer the role's ARN.
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
    accepted_query(
        router,
        "iam",
        "2010-05-08",
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyArn", BASIC_EXECUTION_POLICY),
        ],
    )
    .await;
    role_arn
}

/// Deploy the stack declaring the function: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deploy_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Backend": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": BACKEND_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": BACKEND_HANDLER },
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
    // The invoke the integration makes needs the function RUNNABLE, which is a transition its own
    // service owes: look until it reports it rather than sleeping a span on its behalf.
    let deadline = Instant::now() + BUDGET;
    let configuration = loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{BACKEND_FUNCTION}/configuration"),
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
        "setup: Lambda must report {BACKEND_FUNCTION:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// The group a run of the backend logs to — the default group of a function that names no other.
fn backend_group() -> String {
    format!("/aws/lambda/{BACKEND_FUNCTION}")
}

/// The messages `FilterLogEvents` reports for `group`, or `None` when the account holds no such
/// group at all — which is what "nothing ran, so nothing created it" looks like to a client.
///
/// ONLY the modelled not-found refusal reads as that absence: any other answer is a failure of
/// the read itself, and letting it read as "the group is not there" would let an absence hold for
/// the wrong reason.
async fn delivered(router: &MultiServiceRouter, group: &str) -> Option<Vec<String>> {
    let answered = logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    if answered.status != 200 {
        assert!(
            answered.status == GROUP_ABSENT_STATUS
                && named_error(&answered).contains(GROUP_ABSENT_EXCEPTION),
            "a read of {group:?} must either be served or refused {GROUP_ABSENT_EXCEPTION:?} with \
             HTTP {GROUP_ABSENT_STATUS}, got {}",
            answered.summary()
        );
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
                            .trim()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// The error shape an answer names, however this protocol carries it: the `x-amzn-errortype`
/// header, else the `__type` member of the document.
fn named_error(answered: &Answered) -> String {
    answered.header(ERROR_TYPE_HEADER).unwrap_or_else(|| {
        answered.json()["__type"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    })
}

/// Look in `group` until one message carrying `wanted` is held, and answer it; `None` once the
/// deadline passes with none. The wait is on the CONDITION — a delivery another service makes —
/// so it polls until it holds rather than sleeping a span.
async fn awaited_line(router: &MultiServiceRouter, group: &str, wanted: &str) -> Option<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(messages) = delivered(router, group).await {
            if let Some(found) = messages.into_iter().find(|message| message.contains(wanted)) {
                return Some(found);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The whole wiring, as a deployment of a pool-gated Lambda-backed api builds one: the api, the
/// resource, the user-pool authorizer, the gated method, the proxy integration, the grant that
/// admits the invocation and the deployment that mints the stage. Answers the api id.
async fn deployed(router: &MultiServiceRouter, pool_arn: &str, function_arn: &str) -> String {
    let api_record = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cognito-gated-run-log-api" })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &api_record, "id");
    let root = as_str("CreateRestApi", &api_record, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource_id = as_str("CreateResource", &resource, "id");
    let authorizer = apigw(
        router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "pool-gate",
            "type": "COGNITO_USER_POOLS",
            "providerARNs": [pool_arn],
            "identitySource": format!("method.request.header.{TOKEN_HEADER}"),
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    apigw(
        router,
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
        router,
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
    // The grant a deployment scopes by the api's OWN resources rather than by one method of it:
    // `aws:SourceArn` is matched as a PATTERN, so this one statement admits every stage and every
    // method of this api.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{BACKEND_FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{account}:{api}/*/*",
                account = ::engine::EMULATOR_ACCOUNT_ID
            ),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission must answer 201, got {}",
        granted.summary()
    );
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// WHICH value a leg presents at the authorizer's identity source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// No `Authorization` header at all.
    Nothing,
    /// The header, carrying nothing.
    Empty,
    /// A value that is no token at all, behind the RFC 6750 bearer scheme.
    BearerNotAToken,
    /// The identity token of the pool the authorizer names.
    IdToken,
    /// The access token of that same sign-in.
    AccessToken,
}

/// One request to the deployed stage: what it presents, and whether the gate hands it to the
/// integration.
struct Row {
    case: &'static str,
    presented: Presented,
    /// Whether the method's function RUNS for this leg.
    runs: bool,
}

/// The table, in order. The refusals are driven BOTH before anything has ever run and again once
/// the backend's group holds another request's run, so an absence that only holds because the
/// group is empty fails on the later rows; the last leg driven is an admitted one whose line is
/// awaited, so the group is known to be taking lines at the instant the absences are read.
const ROWS: &[Row] = &[
    Row {
        case: "no Authorization header at all",
        presented: Presented::Nothing,
        runs: false,
    },
    Row {
        case: "an Authorization header carrying nothing",
        presented: Presented::Empty,
        runs: false,
    },
    Row {
        case: "a value that is no token, behind the bearer scheme",
        presented: Presented::BearerNotAToken,
        runs: false,
    },
    Row {
        case: "the identity token the named pool minted",
        presented: Presented::IdToken,
        runs: true,
    },
    Row {
        case: "no Authorization header, now that the group holds another run",
        presented: Presented::Nothing,
        runs: false,
    },
    Row {
        case: "a value that is no token, now that the group holds another run",
        presented: Presented::BearerNotAToken,
        runs: false,
    },
    Row {
        case: "the access token of that same sign-in",
        presented: Presented::AccessToken,
        runs: true,
    },
];

/// A value that is no token at all, as a client presenting a string it took for one sends it.
const NOT_A_TOKEN: &str = "not-a-real-jwt.at.all";

/// RULE: a `COGNITO_USER_POOLS`-gated proxy method answers every request at the edge — `401`
/// `{"message":"Unauthorized"}` with NOTHING of a run in the function's own log group for a value
/// no named pool signed, and the integration's own envelope, one run and that run's whole log,
/// for a token one of them did.
#[test]
fn a_cognito_gated_proxy_method_logs_the_runs_it_admits_and_none_it_refuses() {
    std::thread::Builder::new()
        .name("apigateway-cognito-run-log".to_string())
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

    // 1. The role, the function it runs as, and the pool whose tokens the method accepts.
    let role_arn = create_execution_role(&router).await;
    let function_arn = deploy_function(&router, &role_arn).await;
    let signed_in = signed_in_user(&router).await;

    // 2. The api, deployed to its stage.
    let api = deployed(&router, &signed_in.pool_arn, &function_arn).await;
    let group = backend_group();

    // Nothing has run yet, so the function's group is not a resource its create made: a client
    // reading it is REFUSED the way CloudWatch Logs models a group that does not exist, rather
    // than served an empty page of a group that does. That discrimination is [`delivered`]'s own,
    // so "the group is not there" has ONE definition here.
    let before = delivered(&router, &group).await;
    assert_eq!(
        before, None,
        "before any run, a read of {group:?} must be refused {GROUP_ABSENT_EXCEPTION:?} with HTTP \
         {GROUP_ABSENT_STATUS}, got {before:?}"
    );

    // 3. The legs, in order: each presents its own value at the identity source and names itself
    //    in the marker the handler prints.
    let mut refused: Vec<String> = Vec::new();
    let mut ran: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = format!("leg-{index}");
        let presented: Option<String> = match row.presented {
            Presented::Nothing => None,
            Presented::Empty => Some(String::new()),
            Presented::BearerNotAToken => Some(format!("Bearer {NOT_A_TOKEN}")),
            Presented::IdToken => Some(signed_in.id_token.clone()),
            Presented::AccessToken => Some(signed_in.access_token.clone()),
        };
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(value) = presented.as_deref() {
            headers.push((TOKEN_HEADER, value));
        }
        let answered = dispatch(
            &router,
            "GET",
            &format!("{api}.execute-api.{REGION}.amazonaws.com"),
            &format!("/{STAGE}/{RESOURCE_PATH}?{MARKER_PARAMETER}={marker}"),
            &headers,
            Bytes::new(),
        )
        .await;
        let printed = format!("{BACKEND_MARKER} {marker}");
        if row.runs {
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected the integration's own 200, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.json(),
                json!({ "ran": marker, "sub": signed_in.subject }),
                "case {case:?}: expected the handler's envelope naming the marker and the \
                 admitted token's subject, got {}",
                answered.summary()
            );
            let found = awaited_line(&router, &group, &printed).await;
            assert_eq!(
                found.as_deref(),
                Some(printed.as_str()),
                "case {case:?}: expected the run's own line {printed:?} in {group:?}, got \
                 {found:?} among {:?}",
                delivered(&router, &group).await
            );
            ran.push(marker);
            continue;
        }
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
            "case {case:?}: expected {ERROR_TYPE_HEADER:?} to name {UNAUTHORIZED_EXCEPTION:?}, \
             got {:?}",
            answered.header(ERROR_TYPE_HEADER)
        );
        refused.push(marker);
    }

    // 4. What the group holds once every leg has been answered: one run per admitted leg, framed
    //    by the runtime, and not one line of any refused one.
    let held = delivered(&router, &group)
        .await
        .unwrap_or_else(|| panic!("{group:?} must be readable once the backend has run"));
    let framed = held
        .iter()
        .filter(|message| message.starts_with(START_RECORD))
        .count();
    assert_eq!(
        framed,
        ran.len(),
        "expected one framed run in {group:?} per admitted leg ({}), got {framed} among {held:?}",
        ran.len()
    );
    for marker in &ran {
        let printed = format!("{BACKEND_MARKER} {marker}");
        let runs = held
            .iter()
            .filter(|message| message.contains(printed.as_str()))
            .count();
        assert_eq!(
            runs, 1,
            "expected exactly one run of the backend for the admitted leg {marker:?}, got {runs} \
             line(s) among {held:?}"
        );
    }
    for marker in &refused {
        let printed = format!("{BACKEND_MARKER} {marker}");
        assert!(
            !held.iter().any(|message| message.contains(printed.as_str())),
            "case {marker:?}: a refused request must never reach the integration, yet {printed:?} \
             is in {group:?}: {held:?}"
        );
    }
}
