// SPDX-License-Identifier: CC0-1.0

//! What a client READING THE WIRE is answered by a `COGNITO_USER_POOLS`-gated method of a
//! deployed REST stage, and what that answer left behind in the backend's own log group.
//!
//! THE RULE UNDER TEST, one level above the recorded case:
//!
//! > A request to a deployed method whose `authorizationType` is `COGNITO_USER_POOLS` is
//! > decided at the edge and answered SYNCHRONOUSLY on the connection it arrived on: a value
//! > at the authorizer's `Authorizer$identitySource` that is not a token one of the user pools
//! > its `providerARNs` name signed — nothing presented at all, the header carrying nothing, a
//! > value that is no token, one under the RFC 6750 bearer scheme — authorized nobody, so the
//! > request is answered `401` with the fixed body `{"message":"Unauthorized"}` and the
//! > `UnauthorizedException` class in `x-amzn-errortype`, and the method's integration is NOT
//! > performed, which is observable as nothing of a run of its function reaching that
//! > function's own log group; a token such a pool minted reaches the integration and the
//! > client is served the envelope the function returned. Every one of those answers is a
//! > COMPLETE HTTP response — its status line, its own `content-length` and exactly that many
//! > bytes of body — so a client that keeps the connection open reads the next answer on it
//! > rather than waiting for bytes that never come.
//!
//! Why this suite exists beside [`crate::stage_cognito_gate_spares_its_backend`], which pins
//! the same decisions through the router IN PROCESS: a client of a published invoke URL is a
//! stock HTTP client, and what it gets is the response as the SERVED socket wrote it. An answer
//! whose framing disagrees with its body — a `content-length` naming other than the bytes that
//! follow, a connection closed before the body — is the same object in process and a client
//! that blocks forever on the wire, so the only level at which "the caller is answered at all"
//! is observable is a real socket. The router is served the way this emulator's own entry point
//! serves it (`emulator_server::serve_on_ephemeral_port`), with request authentication ON, as a
//! started emulator verifies signatures: an invoke of a deployed stage is served unsigned, and
//! the value at the identity source is a bearer token rather than a credential. It is the
//! sibling suite of [`crate::stage_authorizer_decision_on_the_served_wire`], which reads a
//! `CUSTOM` authorizer's decision off the same wire, and it reads that wire with that suite's
//! own reader.
//!
//! The fixture is built the way a client SCRIPTS one: the token comes from
//! `AdminInitiateAuth` over `ADMIN_USER_PASSWORD_AUTH` on a pool whose user was created and
//! given a permanent password, the function is created from a `.zip` deployment package of its
//! own and granted to `apigateway.amazonaws.com` by the WILDCARD `aws:SourceArn` of the whole
//! api, and the stage is a `CreateStage` naming a `CreateDeployment` that named no stage —
//! which is the other order the two calls come in.
//!
//! The rows differ along what the rule generalises over: WHAT the request presents at the
//! identity source (nothing, an empty header, a value that is no token, three segments that are
//! no pool's token, the minted identity token, that same token under the bearer scheme), HOW
//! the stage is addressed (the api's own `execute-api` host and the data-plane address a client
//! that resolves that host nowhere reaches this emulator by), and WHETHER the backend's group
//! already holds another request's run when an absence is read — so an absence that only holds
//! because nothing has ever run fails on the later rows. Every row travels on ONE connection,
//! in order, so a row is read only if the row before it was framed.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::AsyncWriteExt as _;

use crate::stage_authorizer_decision_on_the_served_wire::{field_of, read_answer};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, and the resource path of its one gated method.
const STAGE: &str = "test";
const RESOURCE_PATH: &str = "secure";

/// The function the gated method's `AWS_PROXY` integration names, and the group its runs log
/// to — "By default, Lambda functions send logs to a default log group named
/// `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const FUNCTION: &str = "cognito-wire-backend";

/// The role the created function names. No IAM store is mounted here, so it is a value the
/// create carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// The handler the deployment package declares.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the gate's decision rather than how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The pool whose tokens the method accepts, the user who signs in, and the permanent password
/// that sign-in uses — a value written for this suite and used nowhere else, which satisfies a
/// pool's default password policy.
const POOL_NAME: &str = "cognito-wire-pool";
const USERNAME: &str = "wire-user";
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The header the authorizer declares as its identity source, and the query parameter every
/// request carries its own marker in — which is what the handler prints, so one logged line
/// names the row that produced it.
const TOKEN_HEADER: &str = "Authorization";
const MARKER_PARAMETER: &str = "marker";

/// What the gate owes a request that authorized nobody: the status, the body's one member and
/// its fixed sentence, and the class the endpoint names it in.
const UNAUTHORIZED: u16 = 401;
const MESSAGE_MEMBER: &str = "message";
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";
const ERROR_TYPE_FIELD: &str = "x-amzn-errortype";
const UNAUTHORIZED_EXCEPTION: &str = "UnauthorizedException";

/// The sentence the `API_CONFIGURATION_ERROR` / `AUTHORIZER_CONFIGURATION_ERROR` gateway
/// responses carry: the `500` that must never answer a question about a presented token.
const CONFIGURATION_ERROR: &str = "Internal server error";

/// The line the backend prints, which is what a run of it leaves in its own log group, and the
/// record the runtime opens every run's log with.
const BACKEND_MARKER: &str = "BACKEND-INVOKED";
const START_RECORD: &str = "START RequestId: ";

/// A value that is no token at all, as a client presenting a string it took for one sends it.
const NOT_A_TOKEN: &str = "not-a-real-jwt.at.all";

/// Three segments that are not the base64url JSON of a compact JWS, which is a value no pool
/// could have signed however it is read.
const THREE_SEGMENTS: &str = "aaa.bbb.ccc";

/// How long one answer is waited for on the socket, how long work another service's clock owes
/// (a created function becoming `Active`, a run's log reaching its group) is waited for, and
/// how long between two looks. Generous, and on the CONDITION rather than on a span: nothing
/// here is graded on how fast the host runs, and the waits exist only so a stalled read fails
/// the suite instead of hanging the whole test binary.
const ANSWER_WAIT: Duration = Duration::from_secs(60);
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the gated method fronts. It PRINTS the marker the request carried — so a run of
/// it leaves that line in its own log group — and answers a proxy envelope naming that marker
/// and the subject the gate published for the admitted token, so a run is observable to the
/// client too.
const BACKEND_HANDLER: &str = r#"
import json


def lambda_handler(event, context):
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

/// One answered in-process request: its status and its body verbatim.
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

/// The four services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them: the pool whose key set the gate reads is the pool this suite mints, and the group a
/// run's log lands in is the group this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The fixture exercises the services, not SigV4: its control-plane calls carry a credential
    // scope so the router can name the target service. Verification is switched ON before the
    // rows are driven, which is the level the rule is asserted at.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request while verification is off.
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
        "setup {what:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
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
            ("authorization", &authorization("cognito-idp")),
            (
                "x-amz-target",
                &format!("AWSCognitoIdentityProviderService.{operation}"),
            ),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup cognito-idp {operation}: expected HTTP 200, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One AWS Lambda call over its `restJson1` wire, answering its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
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
    .await;
    (answered.status, answered.json())
}

/// One CloudWatch Logs call over its `awsJson1_1` wire, answering its status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let answered = dispatch(
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
    .await;
    (answered.status, answered.json())
}

/// A required non-empty string member of a response body.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {name:?}, got {body}"))
}

/// A .zip deployment package holding `source` as the module the handler names, written in
/// memory the way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// What one pool's sign-in produced: the pool's ARN, the identity token of that sign-in and the
/// subject the pool holds for the user who signed in.
struct SignedIn {
    pool_arn: String,
    id_token: String,
    subject: String,
}

/// Mint one pool, one app client and one confirmed user, and sign that user in — so the token
/// this suite presents is one the pool ITSELF minted and signed.
async fn signed_in_user(router: &MultiServiceRouter) -> SignedIn {
    let pool = cognito(router, "CreateUserPool", &json!({ "PoolName": POOL_NAME })).await;
    let pool_id = member("CreateUserPool", &pool["UserPool"], "Id");
    let pool_arn = member("CreateUserPool", &pool["UserPool"], "Arn");
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
    let client_id = member("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");
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
    let id_token = member(
        "AdminInitiateAuth",
        &authenticated["AuthenticationResult"],
        "IdToken",
    );
    let user = cognito(
        router,
        "AdminGetUser",
        &json!({ "UserPoolId": pool_id, "Username": USERNAME }),
    )
    .await;
    let subject = user["UserAttributes"]
        .as_array()
        .and_then(|attributes| {
            attributes
                .iter()
                .find(|attribute| attribute.get("Name").and_then(Value::as_str) == Some("sub"))
        })
        .map(|attribute| member("AdminGetUser", attribute, "Value"))
        .expect("the created user's own subject attribute");
    SignedIn {
        pool_arn,
        id_token,
        subject,
    }
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`.
///
/// The span a created function spends `Pending` is that service's own, so nothing here sleeps a
/// fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup CreateFunction: the function never became Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The group a run of the backend logs to.
fn backend_group() -> String {
    format!("/aws/lambda/{FUNCTION}")
}

/// Every message `FilterLogEvents` reports for the backend's own group, each as the RECORD's
/// own text — a record is delivered closed by the line break the write that produced it ended
/// with, which belongs to the record rather than to its text.
///
/// A group no run has created yet is REFUSED rather than served an empty page, which reads here
/// as the no messages it is.
async fn delivered(router: &MultiServiceRouter) -> Vec<String> {
    let group = backend_group();
    let (status, body) = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if status != 200 {
        return Vec::new();
    }
    body.get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    let message = event
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    message.strip_suffix('\n').unwrap_or(message).to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look at the backend's group until it holds `line`, or until the budget passes.
///
/// A wait on the CONDITION: the delivery is another service's work, and this suite is not
/// graded on how fast it happens.
async fn awaited_line(router: &MultiServiceRouter, line: &str) -> bool {
    let deadline = Instant::now() + BUDGET;
    loop {
        if delivered(router)
            .await
            .iter()
            .any(|message| message.contains(line))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The whole wiring, as a client scripts it: the function and the grant that admits its
/// invocation, the api with one `COGNITO_USER_POOLS`-gated `GET` in front of it, and a stage
/// naming a deployment that named no stage of its own. Answers the api id.
async fn deployed(router: &MultiServiceRouter, pool_arn: &str) -> String {
    let (status, function) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(BACKEND_HANDLER)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected HTTP 201, got {status} with {function}"
    );
    let function_arn = member("CreateFunction", &function, "FunctionArn");

    let api = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cognito-gate-on-the-served-wire" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api, "id");
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
        .map(|item| member("GetResources", item, "id"))
        .expect("the api's root resource");

    let authorizer = apigw(
        router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "pool-authorizer",
            "type": "COGNITO_USER_POOLS",
            // "A list of the Amazon Cognito user pool ARNs for the COGNITO_USER_POOLS
            // authorizer" (`Authorizer$providerARNs`): the one pool whose tokens this gate
            // accepts.
            "providerARNs": [pool_arn],
            // "The identity source for which authorization is requested"
            // (`Authorizer$identitySource`): the request header the token travels in.
            "identitySource": format!("method.request.header.{TOKEN_HEADER}"),
            // "If it equals 0, authorization caching is disabled": every row's own value is
            // evaluated, so one row cannot be decided by another's held result.
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer = member("CreateAuthorizer", &authorizer, "id");

    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource = member("CreateResource", &resource, "id");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET"),
        Some(json!({
            "authorizationType": "COGNITO_USER_POOLS",
            "authorizerId": authorizer,
        })),
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

    // "Grants a principal permission to use a function" (`AddPermission`), scoped by the
    // `aws:SourceArn` a deployment of a WHOLE api is granted under: every stage and every
    // method of this api.
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke-backend",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{account}:{api}/*/*",
                account = engine::emulator_account_id(),
            ),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission: expected HTTP 201, got {status} with {granted}"
    );

    // The deployment names no stage, and the stage names the deployment: the other order the
    // two calls come in, which deploys the same api to the same stage.
    let deployment = apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "description": "the gated method" })),
        201,
    )
    .await;
    let deployment = member("CreateDeployment", &deployment, "id");
    apigw(
        router,
        "CreateStage",
        "POST",
        &format!("/restapis/{api}/stages"),
        Some(json!({ "stageName": STAGE, "deploymentId": deployment })),
        201,
    )
    .await;

    wait_until_active(router).await;
    api
}

/// WHICH value a row presents at the authorizer's identity source.
#[derive(Debug, Clone, Copy)]
enum Presented {
    /// No `Authorization` field line at all.
    Nothing,
    /// The field line, carrying nothing.
    Empty,
    /// A value that is no token at all, behind the RFC 6750 bearer scheme.
    BearerNotAToken,
    /// Three segments that are no pool's token.
    ThreeSegments,
    /// The identity token the named pool minted.
    IdToken,
    /// That same token under the bearer scheme.
    BearerIdToken,
}

/// How a row addresses the deployed stage: the two addressings this emulator serves one stage
/// at, which the gate's decision cannot depend on.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// The api's own `{api-id}.execute-api.{region}.amazonaws.com` host, which is the address
    /// API Gateway publishes for a deployed stage.
    OwnEndpointHost,
    /// The emulator's own address, with the data plane and the api named in the PATH — how a
    /// client that resolves AWS's own hostnames nowhere reaches the very same stage.
    EndpointInThePath,
}

impl Addressing {
    /// The request target and the `host` field one row sends, given the api, the marker the
    /// request carries and the authority the served socket answers on.
    fn target(self, api: &str, marker: &str, served: &str) -> (String, String) {
        let path = format!("/{STAGE}/{RESOURCE_PATH}?{MARKER_PARAMETER}={marker}");
        match self {
            Self::OwnEndpointHost => (path, format!("{api}.execute-api.{REGION}.amazonaws.com")),
            Self::EndpointInThePath => (
                format!("/_aws/execute-api/{api}{path}"),
                served.to_string(),
            ),
        }
    }
}

/// One row: what it presents at the identity source, how it addresses the stage, and whether
/// the method's function RUNS for it.
struct Row {
    case: &'static str,
    presented: Presented,
    addressing: Addressing,
    runs: bool,
}

/// The table, in the order it travels the one connection. The admitted rows sit BETWEEN refused
/// ones, so the absences read at the end are read of a group that is known to be taking lines,
/// and the last row driven is an admitted one whose line is awaited.
const ROWS: &[Row] = &[
    Row {
        case: "no Authorization field line at all, on the api's own endpoint host",
        presented: Presented::Nothing,
        addressing: Addressing::OwnEndpointHost,
        runs: false,
    },
    Row {
        case: "an Authorization field line carrying nothing, with the endpoint in the path",
        presented: Presented::Empty,
        addressing: Addressing::EndpointInThePath,
        runs: false,
    },
    Row {
        case: "a value that is no token, behind the bearer scheme",
        presented: Presented::BearerNotAToken,
        addressing: Addressing::OwnEndpointHost,
        runs: false,
    },
    Row {
        case: "three segments that are no pool's token, with the endpoint in the path",
        presented: Presented::ThreeSegments,
        addressing: Addressing::EndpointInThePath,
        runs: false,
    },
    Row {
        case: "the identity token the named pool minted",
        presented: Presented::IdToken,
        addressing: Addressing::OwnEndpointHost,
        runs: true,
    },
    Row {
        case: "no Authorization field line, now that the group holds another run",
        presented: Presented::Nothing,
        addressing: Addressing::EndpointInThePath,
        runs: false,
    },
    Row {
        case: "that same token under the bearer scheme, with the endpoint in the path",
        presented: Presented::BearerIdToken,
        addressing: Addressing::EndpointInThePath,
        runs: true,
    },
];

/// RULE: a `COGNITO_USER_POOLS`-gated method of a deployed stage answers every request on the
/// connection it arrived on — `401` `{"message":"Unauthorized"}` with nothing of a run in the
/// backend's own log group for a value no named pool signed, and the integration's own envelope
/// and that run's log for a token one of them minted.
#[tokio::test]
async fn the_cognito_gate_decision_reaches_a_client_reading_the_wire() {
    let mut router = emulator();
    let signed_in = signed_in_user(&router).await;
    let api = deployed(&router, &signed_in.pool_arn).await;

    // Signature verification belongs to the started emulator, and this is the level at which it
    // holds or does not: every row below carries a bearer token at the identity source, or
    // nothing at all, and no credential — as a client of a published invoke URL sends none.
    router.set_request_authentication(true);

    // The SAME router, served by the framework that serves this emulator's own data port. The
    // handle it answers with is that router, which the log group is read back through.
    let (endpoint, mut served) = ::emulator_server::serve_on_ephemeral_port(router).await;
    let port: u16 = endpoint
        .rsplit_once(':')
        .and_then(|(_before, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("no port in the served endpoint {endpoint:?}"));
    let served_authority = format!("127.0.0.1:{port}");
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the served port accepts a connection");

    let mut refused: Vec<String> = Vec::new();
    let mut ran: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = format!("leg-{index}");
        let (target, host) = row.addressing.target(&api, &marker, &served_authority);
        let identity = match row.presented {
            Presented::Nothing => String::new(),
            Presented::Empty => format!("{TOKEN_HEADER}: \r\n"),
            Presented::BearerNotAToken => format!("{TOKEN_HEADER}: Bearer {NOT_A_TOKEN}\r\n"),
            Presented::ThreeSegments => format!("{TOKEN_HEADER}: {THREE_SEGMENTS}\r\n"),
            Presented::IdToken => format!("{TOKEN_HEADER}: {}\r\n", signed_in.id_token),
            Presented::BearerIdToken => {
                format!("{TOKEN_HEADER}: Bearer {}\r\n", signed_in.id_token)
            }
        };
        // The field lines a stock client sends beside the one this row is about: the answer is
        // read on a kept-open connection, which is what every HTTP/1.1 client does.
        let request = format!(
            "GET {target} HTTP/1.1\r\nhost: {host}\r\n{identity}user-agent: emulator-suite\r\n\
             accept: */*\r\naccept-encoding: gzip, deflate\r\nconnection: keep-alive\r\n\r\n"
        );
        socket
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the request is written: {error}"));

        let answered = match ::tokio::time::timeout(ANSWER_WAIT, read_answer(&mut socket)).await {
            Ok(Ok(answered)) => answered,
            Ok(Err(reason)) => panic!("case {case:?}: no complete answer on the wire: {reason}"),
            Err(_elapsed) => panic!(
                "case {case:?}: the stage sent no answer a client could read on the connection \
                 it asked on"
            ),
        };
        let body: Value = ::serde_json::from_str(&answered.body).unwrap_or(Value::Null);

        // Whatever the gate decided, a question about a presented value is never answered as a
        // fault of the gate itself.
        assert!(
            !(500..600).contains(&answered.status),
            "case {case:?}: expected a decision about the presented value, got {} with {:?}",
            answered.status,
            answered.body
        );
        assert_ne!(
            body.get(MESSAGE_MEMBER).and_then(Value::as_str),
            Some(CONFIGURATION_ERROR),
            "case {case:?}: expected no configuration-error sentence, got {:?}",
            answered.body
        );

        if row.runs {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected the integration's own HTTP 200 on the wire, got {} \
                 with {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                body,
                json!({ "ran": marker, "sub": signed_in.subject }),
                "case {case:?}: expected the handler's envelope naming the marker and the \
                 admitted token's subject, got {:?}",
                answered.body
            );
            ran.push(marker);
            continue;
        }
        assert_eq!(
            answered.status, UNAUTHORIZED,
            "case {case:?}: expected HTTP {UNAUTHORIZED} on the wire, got {} with {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            body,
            json!({ MESSAGE_MEMBER: UNAUTHORIZED_MESSAGE }),
            "case {case:?}: expected the fixed refusal body, got {:?}",
            answered.body
        );
        assert_eq!(
            field_of(&answered.head, ERROR_TYPE_FIELD).as_deref(),
            Some(UNAUTHORIZED_EXCEPTION),
            "case {case:?}: expected {ERROR_TYPE_FIELD:?} to name {UNAUTHORIZED_EXCEPTION:?}, \
             got {:?}",
            answered.head
        );
        refused.push(marker);
    }

    // What the backend's own group holds once every row has been answered, read back through
    // CloudWatch Logs' own reader on the same router — with verification off for this suite's
    // own reads, which carry no real signature.
    served.set_request_authentication(false);
    let last_admitted = ran
        .last()
        .map(|marker| format!("{BACKEND_MARKER} {marker}"))
        .expect("the table drives at least one admitted row");
    assert!(
        awaited_line(&served, &last_admitted).await,
        "expected the admitted row's own line {last_admitted:?} in {:?}, got {:?}",
        backend_group(),
        delivered(&served).await
    );

    let held = delivered(&served).await;
    let framed = held
        .iter()
        .filter(|message| message.starts_with(START_RECORD))
        .count();
    assert_eq!(
        framed,
        ran.len(),
        "expected one framed run in {:?} per admitted row ({}), got {framed} among {held:?}",
        backend_group(),
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
            "case {marker:?}: expected exactly one run of the backend for an admitted row, got \
             {runs} line(s) among {held:?}"
        );
    }
    for marker in &refused {
        let printed = format!("{BACKEND_MARKER} {marker}");
        assert!(
            !held.iter().any(|message| message.contains(printed.as_str())),
            "case {marker:?}: a refused request must never reach the integration, yet \
             {printed:?} is in {:?}: {held:?}",
            backend_group()
        );
    }
}
