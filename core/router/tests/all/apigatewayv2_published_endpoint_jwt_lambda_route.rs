// SPDX-License-Identifier: CC0-1.0

//! The JWT-authorizer surface of an HTTP api, driven at the INVOKE ADDRESS that api
//! PUBLISHES, with the route's backend an AWS Lambda function that really runs.
//!
//! RULE: **an api's `ApiEndpoint` is an address THIS emulator serves that api's data plane
//! at — the endpoint the minting request itself reached, with the api behind the reserved
//! data-plane path, for a client that resolves nothing but the emulator's own endpoint, and
//! the api's own `{api-id}.execute-api.{region}.amazonaws.com` host for a client that
//! resolves AWS's own hostnames — reported unchanged by every later reporter; and a request a
//! client builds from that published value (the value with the stage name appended, the
//! reserved stage that serves the api's root being reached with no stage segment at all) is
//! decided there by the route's JWT authorizer: a token whose `iss` is that authorizer's
//! `Issuer`, whose `client_id` is one entry of its `Audience` and which is inside its
//! validity window reaches the route's `AWS_PROXY` function, which RUNS, is handed that
//! token's claims and scopes under `requestContext.authorizer.jwt` and answers the client
//! with the object it returned; a token the authorizer cannot accept at all is `401`
//! `{"message":"Unauthorized"}`, and a token it accepts that claims none of the route's
//! `AuthorizationScopes` is `403` `{"message":"Forbidden"}` — neither of them reaching the
//! function at all.**
//!
//! `Api$ApiEndpoint` is "the URI of the API, of the form
//! `{api-id}.execute-api.{region}.amazonaws.com`. The stage name is typically appended to
//! this URI to form a complete path to a deployed API stage", so the published value is the
//! address a client INVOKES; `Route$AuthorizationScopes` states the scope clause ("the method
//! invocation is authorized if any route scope matches a claimed scope in the access token …
//! otherwise, the invocation is not authorized"), and `Integration$IntegrationType` gives
//! `AWS_PROXY` as "for integrating the route or method request with the Lambda
//! function-invoking action". A published address no client on this machine can reach makes
//! every one of those decisions unobservable: the request is never sent, so there is no
//! status, no body and no event.
//!
//! The rows span what the rule generalises over: the ENDPOINT the control-plane and user-pool
//! client was configured with (an IPv4 loopback literal with a port, a loopback NAME with a
//! port, and each service's own published regional host), the reporter the published value is
//! read from (`CreateApi`, `GetApi` at two endpoints, `GetApis`), the invoke URL built from it
//! (the value on its own and with the stage name appended), where the presented token came
//! from (the api's own app client, a second client of the same pool, a second pool), how it is
//! presented (RFC 6750 bearer credentials, bare, absent), whether a claim was changed (`exp`)
//! and what the route requires (no scope, a scope the token claims, a scope it does not).
//!
//! Five crates have to agree for the rule to hold — Amazon Cognito user pools mints the tokens
//! and publishes the issuer, CloudFormation archives the inline handlers into deployment
//! packages this emulator can run, IAM holds the execution role, AWS Lambda holds the
//! functions and RUNS them, API Gateway v2 mints the api, publishes the address and decides a
//! request on it — and Amazon CloudWatch Logs holds the record of every run, which is where a
//! row asserts that a REFUSED request reached no function. So the suite lives here rather
//! than in any one crate.
//!
//! What the neighbouring suites own and this one does not restate: the gate's own clauses over
//! an `HTTP_PROXY` upstream at every addressing
//! (`apigatewayv2_jwt_gate_at_the_reached_endpoint`), the published address against a Lambda
//! route with no authorizer in front of it
//! (`apigatewayv2_published_endpoint_runs_its_lambda_route`), and the shape of the event an
//! admitted token hands on
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/http_api_jwt_claims_reach_the_integration.rs`).
//! What is added here is the JWT decision AND the function that runs behind it, both at the
//! address the api itself published.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_function_log_destination::{messages_in, REPORT_PREFIX};
use crate::cognito_group_role_mapping::decode_base64url;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one the shared stack-status and log
/// helpers poll in — and so the Region of the pool, of the functions and of the api.
const REGION: &str = "us-east-1";

/// The permanent password this suite's fixture user signs in with: a value written for this
/// test and used nowhere else, satisfying a pool's default password policy (a length, an upper
/// and a lower case letter, a digit and a symbol).
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The execution role the template names, provisioned in IAM before the stack so the
/// declaration names a role that service really holds.
const ROLE_NAME: &str = "apigwv2-jwt-lambda-exec";

/// The reserved stage "that serves an api's root" (`$default`), reached with no stage segment
/// in the path at all.
const ROOT_STAGE: &str = "$default";

/// The identity source the authorizer takes the token from: "for HTTP APIs, use selection
/// expressions prefixed with $, for example, $request.header.Auth"
/// (`Authorizer$IdentitySource`).
const IDENTITY_SOURCE: &str = "$request.header.Authorization";

/// The statuses and sentences the two refusal classes carry.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";
const FORBIDDEN: u16 = 403;
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The status, the header and the marker the admitted route's handler answers with, so a
/// served row is told from a refused one by the FUNCTION's own answer.
const SERVED: u16 = 200;
const RAN_HEADER: &str = "x-ran";
const RAN_MARKER: &str = "the-function";

/// The front door's error code for a request NO service claimed. A published address nothing
/// serves is exactly the defect these rows pin, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// A scope no access token of this suite's pool claims, used as the route scope a token
/// cannot meet. The form is a resource server's `{identifier}/{scope}`.
const UNCLAIMED_SCOPE: &str = "things/read";

/// How long something another service owes — a stack's resources reported ready, a function
/// reported invocable, a run's log delivered — is waited for before the suite gives up. Every
/// wait is on that CONDITION, and the budget is far longer than any span those services
/// choose, so a loaded host is only slower; nothing here asserts on elapsed time.
const READY_BUDGET: Duration = Duration::from_secs(120);

/// The routes the api carries: one per scope configuration the rule generalises over, the
/// first two fronting the function an admitted request reaches and the last two fronting the
/// function NO row may reach.
const CLAIMS_PATH: &str = "/claims";
const OPEN_PATH: &str = "/open";
const SCOPED_PATH: &str = "/scoped";
const GUARDED_PATH: &str = "/guarded";

/// The handler an ADMITTED request reaches: it reports what the authorizer told it about the
/// caller, so the claims a row asserts on are read out of the event the function was handed.
const CLAIMS_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    decided = event["requestContext"]["authorizer"]["jwt"]
    return {
        "statusCode": 200,
        "headers": {"x-ran": "the-function", "Content-Type": "application/json"},
        "body": json.dumps(
            {
                "path": event["rawPath"],
                "iss": decided["claims"]["iss"],
                "client_id": decided["claims"]["client_id"],
                "scopes": decided["scopes"],
            }
        ),
    }
"#;

/// The handler behind the two routes every REFUSED row addresses. It answers normally, so a
/// row that reached it would be served rather than refused, and its log group holds a record
/// of every run it makes — which is how "the integration was never invoked" is observed.
const SPARED_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"x-ran": "the-spared-function"},
        "body": json.dumps({"path": event["rawPath"]}),
    }
"#;

/// One address the pool, the api and the invoke of a case are reached at.
struct Addressing {
    case: &'static str,
    /// The authority the user pools service is dialled at, which is also the base of the
    /// issuer a relying party composes.
    cognito_host: &'static str,
    /// The authority the API Gateway v2 control plane is dialled at.
    control_host: &'static str,
    /// Whether that client resolves nothing but the endpoint it reached, which is what decides
    /// the form of the address the minted api announces.
    reaches_only_the_emulator: bool,
}

const LOOPBACK_ENDPOINT: Addressing = Addressing {
    case: "the recorded case: a client configured with the emulator's loopback endpoint",
    cognito_host: "127.0.0.1:45923",
    control_host: "127.0.0.1:45923",
    reaches_only_the_emulator: true,
};
const NAMED_LOCAL_ENDPOINT: Addressing = Addressing {
    case: "the same endpoint named by its loopback NAME, on another port",
    cognito_host: "localhost:9297",
    control_host: "localhost:9297",
    reaches_only_the_emulator: true,
};
const PUBLISHED_REGIONAL_HOSTS: Addressing = Addressing {
    case: "a client that resolves AWS's own hosts: each service's own regional host",
    cognito_host: "cognito-idp.us-east-1.amazonaws.com",
    control_host: "apigateway.us-east-1.amazonaws.com",
    reaches_only_the_emulator: false,
};

/// What a dialled address answered: its status, the front door's error code, the header the
/// handler sets and the body verbatim.
struct Answer {
    status: u16,
    error_type: Option<String>,
    ran_header: Option<String>,
    body: Bytes,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The `message` member of a JSON body, which is how this data plane words its own
    /// answers.
    fn message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Enough of the answer to identify it in an assertion message.
    fn summary(&self) -> String {
        format!(
            "{} {:?} (x-amzn-errortype: {:?}, {RAN_HEADER}: {:?})",
            self.status,
            self.text(),
            self.error_type,
            self.ran_header
        )
    }
}

/// Amazon Cognito user pools, API Gateway v2, CloudFormation, AWS Lambda, IAM and Amazon
/// CloudWatch Logs over ONE engine, as `core/router/src/lib.rs` mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published address
    // carries no credential at all, as a client of a published HTTP api endpoint does.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::
            EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes a
/// call to that service whatever host it addressed.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request with the host, path, headers and body given.
///
/// The request is handed straight to the router IN PROCESS, so no socket is opened and nothing
/// leaves this test: the authority is only the address the request carries.
async fn dispatch(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
    };
    let error_type = header("x-amzn-errortype");
    let ran_header = header(RAN_HEADER);
    Answer {
        status,
        error_type,
        ran_header,
        body: response.into_body(),
    }
}

/// One `awsJson1_1` call to the Amazon Cognito user pools service at `host`, which must
/// succeed.
async fn cognito(router: &MultiServiceRouter, host: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AWSCognitoIdentityProviderService.{operation}");
    let authorization = credential_scope("cognito-idp");
    let answered = dispatch(
        router,
        host,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: cognito-idp {operation} at {host} must answer 200, got {}",
        answered.summary()
    );
    answered.json()
}

/// One API Gateway v2 control-plane call at `host`, which must answer `expected_status`.
async fn control(
    router: &MultiServiceRouter,
    host: &str,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected_status: u16,
) -> Value {
    let authorization = credential_scope("apigateway");
    let answered = dispatch(
        router,
        host,
        method,
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization),
        ],
        body.map(|body| body.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await;
    assert_eq!(
        answered.status,
        expected_status,
        "setup: {what} at {host} must answer HTTP {expected_status}, got {}",
        answered.summary()
    );
    answered.json()
}

/// One request to the api's PUBLISHED address, as a client of it makes it: unsigned, carrying
/// nothing but the token at the authorizer's identity source (and not even that, for the row
/// that presents none).
async fn invoke(
    router: &MultiServiceRouter,
    authority: &str,
    path: &str,
    presented: Option<&str>,
) -> Answer {
    let headers: Vec<(&str, &str)> = match presented {
        Some(value) => vec![("authorization", value)],
        None => Vec::new(),
    };
    dispatch(router, authority, "GET", path, &headers, Vec::new()).await
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what} must carry a non-empty {member:?}, got {body}"))
}

/// `value` as one base64url segment of a compact JWS.
fn encode_segment(value: &Value) -> String {
    aws_smithy_types::base64::encode(value.to_string().as_bytes())
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

/// The JOSE header and the claim set of a compact JWS: its first two segments.
fn parts_of(token: &str) -> (Value, Value) {
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(
        segments.len(),
        3,
        "a minted token must be a compact JWS of three segments, got {} segment(s)",
        segments.len()
    );
    let read = |segment: &str| -> Value {
        serde_json::from_slice(&decode_base64url(segment))
            .unwrap_or_else(|error| panic!("a minted token's segment must be JSON ({error})"))
    };
    (read(segments[0]), read(segments[1]))
}

/// The token the pool minted, re-encoded with the JOSE header and claim set given and its
/// signature segment kept — so the only difference from the admitted token is what the row
/// changed.
fn rebuilt(token: &str, header: &Value, claims: &Value) -> String {
    let signature = token
        .split('.')
        .next_back()
        .expect("a compact JWS has a signature segment");
    format!(
        "{}.{}.{signature}",
        encode_segment(header),
        encode_segment(claims)
    )
}

/// Mint one pool at `host`, one app client per entry of `client_names` and one user with a
/// permanent password, and answer the pool id, the client ids and the ACCESS TOKEN each
/// client's sign-in produced.
async fn pool_with_signed_in_user(
    router: &MultiServiceRouter,
    host: &str,
    pool_name: &str,
    client_names: &[&str],
    username: &str,
) -> (String, Vec<String>, Vec<String>) {
    let pool = cognito(
        router,
        host,
        "CreateUserPool",
        &json!({ "PoolName": pool_name }),
    )
    .await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");

    let mut client_ids = Vec::new();
    for client_name in client_names {
        let client = cognito(
            router,
            host,
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": pool_id,
                "ClientName": client_name,
                // The flow the sign-in below uses, plus the refresh flow AWS requires beside
                // any other `ALLOW_` flow.
                "ExplicitAuthFlows": [
                    "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                    "ALLOW_REFRESH_TOKEN_AUTH"
                ]
            }),
        )
        .await;
        client_ids.push(as_str(
            "CreateUserPoolClient",
            &client["UserPoolClient"],
            "ClientId",
        ));
    }

    cognito(
        router,
        host,
        "AdminCreateUser",
        &json!({ "UserPoolId": pool_id, "Username": username }),
    )
    .await;
    cognito(
        router,
        host,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true
        }),
    )
    .await;

    let mut access_tokens = Vec::new();
    for client_id in &client_ids {
        let signed_in = cognito(
            router,
            host,
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD }
            }),
        )
        .await;
        access_tokens.push(as_str(
            "AdminInitiateAuth",
            &signed_in["AuthenticationResult"],
            "AccessToken",
        ));
    }

    (pool_id, client_ids, access_tokens)
}

/// What the role the functions run as may do: the three CloudWatch Logs calls an invocation's
/// log delivery makes, which is what the AWS-managed basic execution policy grants a function.
/// A run's record is what a refused row observes the ABSENCE of, so the role that would write
/// it has to be allowed to.
fn logs_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Create the execution role the template names, where IAM holds it: trusted by Lambda, which
/// is what lets that service be handed the role at all, and allowed to write its own log.
async fn execution_role(router: &MultiServiceRouter) {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "write-its-own-log"),
            ("PolicyDocument", &logs_policy()),
        ],
    )
    .await;
}

/// The log group a function's runs are recorded in: "a default log group named
/// `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
fn log_group(function: &str) -> String {
    format!("/aws/lambda/{function}")
}

/// The template the two functions are deployed as: the role IAM holds, the inline handlers,
/// and the ARNs the stack publishes for them.
fn function_stack(served: &str, spared: &str) -> String {
    json!({
        "Resources": {
            "Served": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": served,
                    "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 10,
                    "Code": { "ZipFile": CLAIMS_SOURCE },
                },
            },
            "Spared": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": spared,
                    "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 10,
                    "Code": { "ZipFile": SPARED_SOURCE },
                },
            },
        },
        "Outputs": {
            "ServedArn": { "Value": { "Fn::GetAtt": ["Served", "Arn"] } },
            "SparedArn": { "Value": { "Fn::GetAtt": ["Spared", "Arn"] } },
        },
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `function` `Active` — "the
/// function is ready to be invoked" (`FunctionConfiguration$State`). The span is that
/// service's own, so nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, function: &str) {
    let deadline = Instant::now() + READY_BUDGET;
    loop {
        let (_status, read) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        if read.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {function} invocable, last read {read}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Grant API Gateway the invocation of `function` — the `AddPermission` a client makes beside
/// the api it fronts a route with, without which the gateway's invocation is not made at all.
async fn gateway_may_invoke(router: &MultiServiceRouter, case: &str, function: &str) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": "apigateway-may-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission on {function} answered {status} {granted}"
    );
}

/// Deploy both handlers as one stack and answer their ARNs, once Lambda reports each
/// invocable and admits the gateway's invocation. A template is what archives inline source
/// into a deployment package this emulator can run, which is how a handler written out here
/// comes to be a function that RUNS.
async fn given_functions(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    served: &str,
    spared: &str,
) -> (String, String) {
    cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &function_stack(served, spared)),
        ],
    )
    .await;
    let (status, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: expected the functions' stack to deploy, got {status} with {described}"
    );
    let outputs = outputs_of(&described);
    let arn_of = |output: &str| {
        outputs.get(output).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish {output}, got {described}")
        })
    };
    for function in [served, spared] {
        wait_until_active(router, case, function).await;
        gateway_may_invoke(router, case, function).await;
    }
    (arn_of("ServedArn"), arn_of("SparedArn"))
}

/// The announced endpoint split into the authority a client dials and the path it carries
/// ahead of the stage — empty for an endpoint that is a bare host.
fn announced_parts(announced: &str) -> (String, String) {
    let without_scheme = announced
        .split_once("://")
        .map_or(announced, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), String::new()),
    }
}

/// Assert `announced` is the address this emulator serves the row's api at, and answer the
/// authority and base path a client builds its invoke URL from: a client that resolves nothing
/// but the emulator's own endpoint is announced that endpoint with the api behind the reserved
/// data-plane path, and a client that resolves AWS's own hostnames is announced the api's own
/// documented `execute-api` host, which this emulator's front door claims host-style.
fn assert_announced(
    addressing: &Addressing,
    reporter: &str,
    api_id: &str,
    announced: &str,
) -> (String, String) {
    let case = addressing.case;
    assert!(
        announced.starts_with("https://"),
        "case {case:?}: {reporter} must announce an https:// endpoint for an HTTP api, got \
         {announced:?}"
    );
    let (authority, base_path) = announced_parts(announced);
    let (expected_authority, expected_base_path) = if addressing.reaches_only_the_emulator {
        (
            addressing.control_host.to_string(),
            format!("/_aws/execute-api/{api_id}"),
        )
    } else {
        (
            format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
            String::new(),
        )
    };
    assert_eq!(
        authority, expected_authority,
        "case {case:?}: {reporter} must announce the api on {expected_authority}, got \
         {announced:?}"
    );
    assert_eq!(
        base_path, expected_base_path,
        "case {case:?}: {reporter} must announce the base path {expected_base_path:?}, got \
         {announced:?}"
    );
    (authority, base_path)
}

/// Every invoke URL a client builds from the published endpoint for a path within the stage
/// that serves the api's root: the endpoint on its own — the "typically" of `Api$ApiEndpoint` —
/// and the endpoint with the stage name appended.
fn invoke_forms(base_path: &str, path: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "the published endpoint on its own, for the stage that serves the api's root",
            format!("{base_path}{path}"),
        ),
        (
            "the published endpoint with the stage name appended",
            format!("{base_path}/{ROOT_STAGE}{path}"),
        ),
    ]
}

/// What the gate owes one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The gate admitted the request and the route's `AWS_PROXY` function ran.
    Served,
    /// The authorizer accepted the token and the route requires a scope it does not claim.
    Forbidden,
    /// The authorizer could not accept the presented token at all.
    Unauthorized,
}

/// One request to the deployed stage: the path within it, the value presented at the
/// authorizer's identity source (`None` to present nothing at all), and what the gate owes it.
struct Row {
    case: String,
    path: &'static str,
    presented: Option<String>,
    expected: Expected,
}

/// The whole rule at ONE address: the pools and their app clients, the functions, the api with
/// its JWT routes, the published value every reporter reports, and the table of requests a
/// client builds from it.
///
/// One test per address rather than one test walking them all — each address is an independent
/// fixture, so they run in parallel and no single test carries all three.
async fn the_jwt_surface_at(addressing: &Addressing) {
    let case = addressing.case;
    let router = router();
    execution_role(&router).await;
    let served_function = "apigwv2-jwt-claims-echo";
    let spared_function = "apigwv2-jwt-spared";
    let (served_arn, spared_arn) = given_functions(
        &router,
        case,
        "apigwv2-jwt-lambda-route",
        served_function,
        spared_function,
    )
    .await;

    // A pool of ANOTHER issuer: its tokens name a pool no authorizer here records, so they are
    // foreign however they are presented.
    let (_foreign_pool, _foreign_clients, foreign_tokens) = pool_with_signed_in_user(
        &router,
        addressing.cognito_host,
        "jwt-foreign-pool",
        &["foreign-client"],
        "foreign-user",
    )
    .await;
    let foreign_token = foreign_tokens[0].clone();

    // The pool whose tokens the api accepts, with two app clients, created and signed in at
    // THIS address.
    let (pool_id, client_ids, access_tokens) = pool_with_signed_in_user(
        &router,
        addressing.cognito_host,
        "jwt-lambda-pool",
        &["admitted-client", "second-client"],
        "lambda-user",
    )
    .await;
    let admitted_token = access_tokens[0].clone();
    let second_client_token = access_tokens[1].clone();

    // The issuer a relying party composes: the endpoint it dialled, with the pool's id as the
    // path — which for a caller that resolves AWS's own hostnames is the documented
    // `https://cognito-idp.{region}.amazonaws.com/{userPoolId}` (`JWTConfiguration$Issuer`),
    // and for a client of this emulator is the address it can actually reach.
    let issuer = format!("https://{}/{pool_id}", addressing.cognito_host);
    let (header, claims) = parts_of(&admitted_token);
    assert_eq!(
        claims.get("iss").and_then(Value::as_str),
        Some(issuer.as_str()),
        "case {case:?}: the pool's access token must name the issuer a client composes from \
         the endpoint it created the pool through, got {claims}"
    );
    // An ACCESS token names its client in `client_id` rather than in `aud`, which is the
    // audience a JWT authorizer of a user pool matches.
    assert_eq!(
        claims.get("client_id").and_then(Value::as_str),
        Some(client_ids[0].as_str()),
        "case {case:?}: the access token must name the client it was minted for, got {claims}"
    );
    // "a JSON string containing a space-separated list of scopes" (RFC 8693 §4.2); a route
    // below requires one of its entries, so the matching scope is the token's own rather than
    // a value written here.
    let claimed_scope = claims
        .get("scope")
        .and_then(Value::as_str)
        .and_then(|scope| scope.split_whitespace().next())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the access token must carry a scope claim, got {claims}")
        })
        .to_string();
    assert_ne!(
        claimed_scope, UNCLAIMED_SCOPE,
        "case {case:?}: the unmatched route scope must be one the token does not claim"
    );

    // The api, its authorizer, its two integrations, its routes and the stage that serves its
    // root — all created at this address.
    let api = control(
        &router,
        addressing.control_host,
        "CreateApi",
        "POST",
        "/v2/apis",
        Some(json!({ "name": "jwt-lambda-api", "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api_id = as_str("CreateApi", &api, "apiId");
    let announced = as_str("CreateApi", &api, "apiEndpoint");

    let authorizer = control(
        &router,
        addressing.control_host,
        "CreateAuthorizer",
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(json!({
            "name": "pool-authorizer",
            "authorizerType": "JWT",
            "identitySource": [IDENTITY_SOURCE],
            "jwtConfiguration": { "issuer": issuer, "audience": [client_ids[0]] }
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "authorizerId");

    let mut integration_ids = Vec::new();
    for arn in [&served_arn, &spared_arn] {
        let integration = control(
            &router,
            addressing.control_host,
            "CreateIntegration(AWS_PROXY)",
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": arn,
                "payloadFormatVersion": "2.0",
            })),
            201,
        )
        .await;
        integration_ids.push(as_str("CreateIntegration", &integration, "integrationId"));
    }
    let (served_integration, spared_integration) = (&integration_ids[0], &integration_ids[1]);

    // One route per scope configuration: the two an admitted request reaches front the
    // reporting handler, the two every refused row addresses front the handler no row may
    // reach.
    let routes: Vec<(String, &String, Vec<String>)> = vec![
        (
            format!("GET {CLAIMS_PATH}"),
            served_integration,
            vec![claimed_scope.clone()],
        ),
        (format!("GET {OPEN_PATH}"), served_integration, Vec::new()),
        (
            format!("GET {SCOPED_PATH}"),
            spared_integration,
            vec![UNCLAIMED_SCOPE.to_string()],
        ),
        (format!("GET {GUARDED_PATH}"), spared_integration, Vec::new()),
    ];
    for (route_key, integration_id, scopes) in &routes {
        let mut request = json!({
            "routeKey": route_key,
            "target": format!("integrations/{integration_id}"),
            "authorizationType": "JWT",
            "authorizerId": authorizer_id
        });
        if !scopes.is_empty() {
            request["authorizationScopes"] = json!(scopes);
        }
        control(
            &router,
            addressing.control_host,
            &format!("CreateRoute({route_key})"),
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(request),
            201,
        )
        .await;
    }

    let stage = control(
        &router,
        addressing.control_host,
        "CreateStage",
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": ROOT_STAGE, "autoDeploy": true })),
        201,
    )
    .await;
    assert!(
        stage.get("deploymentId").and_then(Value::as_str).is_some(),
        "case {case:?}: an auto-deploying stage must carry a deployment, got {stage}"
    );

    let (authority, base_path) = assert_announced(addressing, "CreateApi", &api_id, &announced);

    // The value is settled by the operation that MINTED the api, so every later reporter
    // reports it unchanged — read at the endpoint the api was created through, at another
    // endpoint altogether, and off the listing.
    for (reporter, reached_at) in [
        ("GetApi at the endpoint the api was created through", addressing.control_host),
        ("GetApi at another endpoint", "apigateway.eu-west-1.amazonaws.com"),
    ] {
        let fetched = control(
            &router,
            reached_at,
            reporter,
            "GET",
            &format!("/v2/apis/{api_id}"),
            None,
            200,
        )
        .await;
        assert_eq!(
            as_str(reporter, &fetched, "apiEndpoint"),
            announced,
            "case {case:?}: {reporter} must report the announced ApiEndpoint {announced:?}, got \
             {fetched}"
        );
        assert_announced(addressing, reporter, &api_id, &as_str(reporter, &fetched, "apiEndpoint"));
    }
    let listed = control(
        &router,
        addressing.control_host,
        "GetApis",
        "GET",
        "/v2/apis",
        None,
        200,
    )
    .await;
    let held = listed["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: GetApis served no items, got {listed}"))
        .iter()
        .find(|item| item.get("apiId").and_then(Value::as_str) == Some(api_id.as_str()))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetApis did not list {api_id}, got {listed}"));
    assert_eq!(
        as_str("GetApis", &held, "apiEndpoint"),
        announced,
        "case {case:?}: GetApis must list the announced ApiEndpoint {announced:?}, got {held}"
    );

    // The one claim the expired row changes: the instant the token was ISSUED at, which is in
    // the past, so a token carrying it as its `exp` is one no gate may accept. Absent, the mint
    // shape changed and the row would be asserting nothing.
    let issued_at = claims.get("iat").and_then(Value::as_i64).unwrap_or_else(|| {
        panic!("case {case:?}: the access token must carry an iat claim, got {claims}")
    });
    let mut expired_claims = claims.clone();
    expired_claims["exp"] = json!(issued_at);
    let bearer = |token: &str| Some(format!("Bearer {token}"));
    let rows = [
        Row {
            case: "the pool's own access token at a route requiring a scope it claims"
                .to_string(),
            path: CLAIMS_PATH,
            presented: bearer(&admitted_token),
            expected: Expected::Served,
        },
        Row {
            case: "the same token presented bare, with no scheme prefix".to_string(),
            path: CLAIMS_PATH,
            presented: Some(admitted_token.clone()),
            expected: Expected::Served,
        },
        Row {
            case: "a route that configures no scope at all".to_string(),
            path: OPEN_PATH,
            presented: bearer(&admitted_token),
            expected: Expected::Served,
        },
        Row {
            case: "a route scope the token's scope claim does not carry".to_string(),
            path: SCOPED_PATH,
            presented: bearer(&admitted_token),
            expected: Expected::Forbidden,
        },
        Row {
            case: "a second client of the same pool, outside the authorizer's one-entry Audience"
                .to_string(),
            path: GUARDED_PATH,
            presented: bearer(&second_client_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "an access token of another user pool: a foreign issuer".to_string(),
            path: GUARDED_PATH,
            presented: bearer(&foreign_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "the pool's own token with its exp no longer in the future".to_string(),
            path: GUARDED_PATH,
            presented: bearer(&rebuilt(&admitted_token, &header, &expired_claims)),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "nothing presented at the authorizer's identity source".to_string(),
            path: GUARDED_PATH,
            presented: None,
            expected: Expected::Unauthorized,
        },
    ];

    // The refused rows run FIRST, so the log group of the function they front has every chance
    // to hold a record by the time the served rows' own records are waited for below.
    let (refused, served): (Vec<&Row>, Vec<&Row>) = rows
        .iter()
        .partition(|row| row.expected != Expected::Served);
    for row in refused.into_iter().chain(served) {
        for (form, path) in invoke_forms(&base_path, row.path) {
            let full_case = format!("{case} / {} / {form}", row.case);
            let answered = invoke(&router, &authority, &path, row.presented.as_deref()).await;
            assert_ne!(
                answered.error_type.as_deref(),
                Some(UNKNOWN_SERVICE),
                "case {full_case:?}: the published address must be one this emulator serves, \
                 but {authority}{path} is claimed by no service: {}",
                answered.summary()
            );
            match row.expected {
                Expected::Served => {
                    assert_eq!(
                        answered.status,
                        SERVED,
                        "case {full_case:?}: an admitted request must be answered by the \
                         route's own function, got {}",
                        answered.summary()
                    );
                    assert_eq!(
                        answered.ran_header.as_deref(),
                        Some(RAN_MARKER),
                        "case {full_case:?}: the answer must carry the header the function \
                         itself set, got {}",
                        answered.summary()
                    );
                    let reported = answered.json();
                    let reached = reported
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    assert!(
                        reached.ends_with(row.path),
                        "case {full_case:?}: the function must have been invoked for {}, got {}",
                        row.path,
                        answered.summary()
                    );
                    // What the ADMITTED token handed the function: the authorizer's own
                    // decision, as `requestContext.authorizer.jwt` carries it.
                    assert_eq!(
                        reported.get("iss").and_then(Value::as_str),
                        Some(issuer.as_str()),
                        "case {full_case:?}: the event must carry the accepted token's issuer \
                         {issuer:?}, got {}",
                        answered.summary()
                    );
                    assert_eq!(
                        reported.get("client_id").and_then(Value::as_str),
                        Some(client_ids[0].as_str()),
                        "case {full_case:?}: the event must carry the accepted token's own \
                         client {:?}, got {}",
                        client_ids[0],
                        answered.summary()
                    );
                    let scopes: Vec<String> = reported
                        .get("scopes")
                        .and_then(Value::as_array)
                        .map(|scopes| {
                            scopes
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    assert!(
                        scopes.contains(&claimed_scope),
                        "case {full_case:?}: the event must carry the token's scope \
                         {claimed_scope:?}, got {scopes:?} in {}",
                        answered.summary()
                    );
                }
                Expected::Forbidden | Expected::Unauthorized => {
                    let (status, message) = if row.expected == Expected::Forbidden {
                        (FORBIDDEN, FORBIDDEN_MESSAGE)
                    } else {
                        (UNAUTHORIZED, UNAUTHORIZED_MESSAGE)
                    };
                    assert_eq!(
                        answered.status,
                        status,
                        "case {full_case:?}: expected HTTP {status}, got {}",
                        answered.summary()
                    );
                    assert_eq!(
                        answered.message(),
                        message,
                        "case {full_case:?}: expected the {message:?} body, got {}",
                        answered.summary()
                    );
                    assert_eq!(
                        answered.ran_header, None,
                        "case {full_case:?}: a refused request must not be answered by a \
                         function at all, got {}",
                        answered.summary()
                    );
                }
            }
        }
    }

    // The served rows' function RAN: its own log group holds the closing frame AWS Lambda
    // writes for every run it finishes. Waiting for that record is also the sync point the
    // assertion below needs — a refused row's invocation, had one been made, would have been
    // recorded before it.
    let served_group = log_group(served_function);
    let deadline = Instant::now() + READY_BUDGET;
    loop {
        let held = messages_in(&router, &served_group).await;
        if held.iter().any(|line| line.starts_with(REPORT_PREFIX)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the admitted requests' function must have logged a run in \
             {served_group:?}, held {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // And the function behind every REFUSED row ran not once: neither refusal reaches the
    // route's integration, so nothing was ever logged for it.
    let spared_records = messages_in(&router, &log_group(spared_function)).await;
    assert!(
        spared_records.is_empty(),
        "case {case:?}: a refused request must not reach the route's integration, but the \
         function behind the refused routes logged {spared_records:?}"
    );
}

#[tokio::test]
async fn the_published_endpoint_decides_its_jwt_routes_at_a_loopback_endpoint() {
    the_jwt_surface_at(&LOOPBACK_ENDPOINT).await;
}

#[tokio::test]
async fn the_published_endpoint_decides_its_jwt_routes_at_a_named_local_endpoint() {
    the_jwt_surface_at(&NAMED_LOCAL_ENDPOINT).await;
}

#[tokio::test]
async fn the_published_endpoint_decides_its_jwt_routes_at_the_published_regional_hosts() {
    the_jwt_surface_at(&PUBLISHED_REGIONAL_HOSTS).await;
}
