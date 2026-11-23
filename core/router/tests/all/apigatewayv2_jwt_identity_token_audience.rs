// SPDX-License-Identifier: CC0-1.0

//! A JWT-gated route of an HTTP api decided by the AUDIENCE of a user pool's IDENTITY token,
//! with the route's backend an AWS Lambda function that really runs.
//!
//! RULE: **a deployed route whose `AuthorizationType` is `JWT` admits a request only when the
//! token at its authorizer's identity source names that authorizer's `Issuer` in `iss` and one
//! entry of its `Audience` in the claim that carries the token's recipient — `aud` for an
//! identity token, `client_id` for an access token, which carries no `aud` — whatever the
//! position of the matching entry and however many entries the list holds; such a request
//! reaches the route's `AWS_PROXY` function, which RUNS and answers the client with the object
//! it returned, and anything else is `401` `{"message":"Unauthorized"}` with the route's
//! function never invoked at all.**
//!
//! `JWTConfiguration$Audience` is "a list of the intended recipients of the JWT. A valid JWT
//! must provide an `aud` that matches at least one entry in this list", and
//! `JWTConfiguration$Issuer` is "the base domain of the identity provider … an Amazon Cognito
//! user pool has the following format: `https://cognito-idp.{region}.amazonaws.com/{userPoolId}`";
//! `Integration$IntegrationType` gives `AWS_PROXY` as "for integrating the route or method
//! request with the Lambda function-invoking action". A pool's `AdminInitiateAuth` answers an
//! `IdToken` beside its `AccessToken`, and it is the IDENTITY token that carries the app client
//! id as `aud` — so the audience clause is only observable end to end when such a token is the
//! one presented.
//!
//! The rows span what the rule generalises over: which claim carries the audience (`aud` of an
//! identity token, `client_id` of an access token), how many entries the authorizer's `Audience`
//! holds and which one matches (the only entry, the first of two, the second of two), which pool
//! and app client the presented token was minted for (the listed client of the authorizer's own
//! pool, a second client of that same pool, a client of a SECOND pool whose own authorizer gates
//! its own route), how the token is presented (RFC 6750 bearer credentials, bare, absent) and
//! which authorizer's route it is presented at.
//!
//! Six crates have to agree for the rule to hold — Amazon Cognito user pools mints the tokens
//! and publishes the issuer, CloudFormation archives the inline handlers into deployment
//! packages this emulator can run, IAM holds the execution role, AWS Lambda holds the functions
//! and RUNS them, API Gateway v2 mints the api and decides each request on its published
//! endpoint, and Amazon CloudWatch Logs holds the record of every run, which is where the rows
//! assert that a REFUSED request reached no function. So the suite lives here rather than in any
//! one crate.
//!
//! What the neighbouring suites own and this one does not restate: the gate's clauses over an
//! `HTTP_PROXY` upstream at every addressing (`apigatewayv2_jwt_gate_at_the_reached_endpoint`),
//! the published address against a Lambda route (`apigatewayv2_published_endpoint_jwt_lambda_route`,
//! which presents ACCESS tokens only) and the scope clause of an admitted token
//! (`apigatewayv2_cognito_jwt_authorizer`). What is added here is the AUDIENCE clause over
//! identity tokens, several `Audience` list shapes and two pools each gating their own route.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigatewayv2_published_endpoint_runs_its_lambda_route::announced_parts;
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_function_log_destination::{
    logs_policy, messages_in, messages_until_condition, wait_until_active, REPORT_PREFIX,
};
use crate::cognito_group_role_mapping::claims_of;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one the shared stack-status helper polls
/// in — and so the Region of the pools, of the functions and of the api.
const REGION: &str = "us-east-1";

/// The host the user pools service is dialled at, which is also the base of the issuer a
/// relying party composes for a pool created through it.
const COGNITO_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The host the API Gateway v2 control plane is dialled at.
const CONTROL_HOST: &str = "apigateway.us-east-1.amazonaws.com";

/// The permanent password this suite's fixture users sign in with: a value written for this
/// test and used nowhere else, satisfying a pool's default password policy (a length, an upper
/// and a lower case letter, a digit and a symbol).
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The execution role the template names, provisioned in IAM before the stack so the
/// declaration names a role that service really holds.
const ROLE_NAME: &str = "apigwv2-id-token-exec";

/// The reserved stage "that serves an api's root" (`$default`), created with auto-deploy on as
/// the recorded client sequence creates it.
const ROOT_STAGE: &str = "$default";

/// The identity source each authorizer takes the token from: "for HTTP APIs, use selection
/// expressions prefixed with $, for example, $request.header.Auth"
/// (`Authorizer$IdentitySource`).
const IDENTITY_SOURCE: &str = "$request.header.Authorization";

/// The status and sentence a token no authorizer of the addressed route can accept earns.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The status, header and marker the admitted routes' handler answers with, so a served row is
/// told from a refused one by the FUNCTION's own answer.
const SERVED: u16 = 200;
const RAN_HEADER: &str = "x-ran";
const RAN_MARKER: &str = "the-function";

/// The front door's error code for a request NO service claimed: the published invoke address
/// is one this emulator serves, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// How long something another service owes — a stack's resources reported ready, a function
/// reported invocable, a stage reported serving, a run's log delivered — is waited for before
/// the suite gives up. Every wait is on that CONDITION, and the budget is far longer than any
/// span those services choose, so a loaded host is only slower; nothing here asserts on
/// elapsed time.
const READY_BUDGET: Duration = Duration::from_secs(120);

/// The paths the api's routes are keyed on: the three an admitted token reaches front the
/// reporting handler, and the two every refused row addresses front the handler no row may
/// reach.
const ONE_ENTRY_PATH: &str = "/one-entry";
const TWO_ENTRY_PATH: &str = "/two-entries";
const SECOND_POOL_PATH: &str = "/second-pool";
const GUARDED_PATH: &str = "/guarded";
const SECOND_POOL_GUARDED_PATH: &str = "/second-pool-guarded";

/// The handler an ADMITTED request reaches: it reports what the authorizer told it about the
/// caller, so the claims a row asserts on are read out of the event the function was handed
/// rather than out of anything this test wrote.
const CLAIMS_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    claims = event["requestContext"]["authorizer"]["jwt"]["claims"]
    return {
        "statusCode": 200,
        "headers": {"x-ran": "the-function", "Content-Type": "application/json"},
        "body": json.dumps(
            {
                "path": event["rawPath"],
                "iss": claims.get("iss"),
                "aud": claims.get("aud"),
                "client_id": claims.get("client_id"),
                "token_use": claims.get("token_use"),
            }
        ),
    }
"#;

/// The handler behind the routes every REFUSED row addresses. It answers normally, so a row
/// that reached it would be served rather than refused, and its log group holds a record of
/// every run it makes — which is how "the integration was never invoked" is observed.
const SPARED_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"x-ran": "the-spared-function"},
        "body": json.dumps({"path": event["rawPath"]}),
    }
"#;

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

/// Amazon Cognito user pools, API Gateway v2, CloudFormation, AWS Lambda, Amazon CloudWatch
/// Logs and IAM over ONE engine, as `core/router/src/lib.rs` mounts them.
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

/// One `awsJson1_1` call to the Amazon Cognito user pools service, which must succeed.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("AWSCognitoIdentityProviderService.{operation}");
    let authorization = credential_scope("cognito-idp");
    let answered = dispatch(
        router,
        COGNITO_HOST,
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
        "setup: cognito-idp {operation} must answer 200, got {}",
        answered.summary()
    );
    answered.json()
}

/// One API Gateway v2 control-plane call, which must answer `expected_status`.
async fn control(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected_status: u16,
) -> Value {
    let authorization = credential_scope("apigateway");
    let answered = dispatch(
        router,
        CONTROL_HOST,
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
        "setup: {what} must answer HTTP {expected_status}, got {}",
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

/// One app client of a pool, with the tokens its sign-in produced.
struct SignedIn {
    client_id: String,
    id_token: String,
    access_token: String,
}

/// One user pool: the issuer a relying party composes for it, and one signed-in app client per
/// name asked for.
struct Pool {
    issuer: String,
    clients: Vec<SignedIn>,
}

/// Mint one pool, one app client per entry of `client_names` and one user with a permanent
/// password, then sign that user in through every client and answer the issuer and the tokens.
async fn pool_with_signed_in_user(
    router: &MultiServiceRouter,
    pool_name: &str,
    client_names: &[&str],
    username: &str,
) -> Pool {
    let pool = cognito(router, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");

    let mut client_ids = Vec::new();
    for client_name in client_names {
        let client = cognito(
            router,
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

    // The issuer a relying party composes: the endpoint the pool was created through with the
    // pool's id as the path, which for a caller that resolves AWS's own hostnames is the
    // documented `https://cognito-idp.{region}.amazonaws.com/{userPoolId}`.
    let issuer = format!("https://{COGNITO_HOST}/{pool_id}");
    let mut clients = Vec::new();
    for client_id in client_ids {
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
        let id_token = as_str("AdminInitiateAuth", result, "IdToken");
        let access_token = as_str("AdminInitiateAuth", result, "AccessToken");

        // The two claims the rows are stated about are the pool's own, not values written
        // here: the identity token names the app client in `aud`, the access token in
        // `client_id`. Absent either, the mint shape changed and the rows below would be
        // asserting about a token no pool produces.
        let identity = claims_of(&id_token);
        assert_eq!(
            identity.get("iss").and_then(Value::as_str),
            Some(issuer.as_str()),
            "the identity token must name the issuer a relying party composes, got {identity}"
        );
        assert_eq!(
            identity.get("aud").and_then(Value::as_str),
            Some(client_id.as_str()),
            "the identity token must name its app client in aud, got {identity}"
        );
        let access = claims_of(&access_token);
        assert_eq!(
            access.get("client_id").and_then(Value::as_str),
            Some(client_id.as_str()),
            "the access token must name its app client in client_id, got {access}"
        );
        assert_eq!(
            access.get("aud").and_then(Value::as_str),
            None,
            "an access token carries no aud, which is why client_id is read instead, got {access}"
        );

        clients.push(SignedIn {
            client_id,
            id_token,
            access_token,
        });
    }

    Pool { issuer, clients }
}

/// Create the execution role the template names, where IAM holds it: trusted by Lambda, which
/// is what lets that service be handed the role at all, and allowed to write its own log — the
/// three CloudWatch Logs calls an invocation's log delivery makes, as the sibling suite that
/// reads a function's own log already states them. A run's record is what a refused row
/// observes the ABSENCE of, so the role that would write it has to be allowed to.
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
            ("PolicyDocument", &logs_policy().to_string()),
        ],
    )
    .await;
}

/// The log group a function's runs are recorded in: "a default log group named
/// `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
fn log_group(function: &str) -> String {
    format!("/aws/lambda/{function}")
}

/// The template the two functions are deployed as: the role IAM holds, the inline handlers, and
/// the ARNs the stack publishes for them.
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

/// Grant API Gateway the invocation of `function` — the `AddPermission` a client makes beside
/// the api it fronts a route with, without which the gateway's invocation is not made at all.
async fn gateway_may_invoke(router: &MultiServiceRouter, function: &str) {
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
        "setup: AddPermission on {function} answered {status} {granted}"
    );
}

/// Deploy both handlers as one stack and answer their ARNs, once Lambda reports each invocable
/// and admits the gateway's invocation. A template is what archives inline source into a
/// deployment package this emulator can run, which is how a handler written out here comes to
/// be a function that RUNS.
async fn given_functions(
    router: &MultiServiceRouter,
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
        "setup: expected the functions' stack to deploy, got {status} with {described}"
    );
    let outputs = outputs_of(&described);
    let arn_of = |output: &str| {
        outputs.get(output).cloned().unwrap_or_else(|| {
            panic!("setup: expected the stack to publish {output}, got {described}")
        })
    };
    for function in [served, spared] {
        // Lambda's own readiness — "the function is ready to be invoked"
        // (`FunctionConfiguration$State`) — waited for through the sibling suite's poller, so
        // nothing here sleeps a fixed span on that service's behalf.
        let reported = wait_until_active(router, "the functions behind the api's routes", function)
            .await;
        assert_eq!(
            reported.get("State").and_then(Value::as_str),
            Some("Active"),
            "setup: Lambda never reported {function} invocable, last read {reported}"
        );
        gateway_may_invoke(router, function).await;
    }
    (arn_of("ServedArn"), arn_of("SparedArn"))
}

/// What the gate owes one row.
enum Expected {
    /// The gate admitted the request and the route's `AWS_PROXY` function ran, having been
    /// handed the token the pool minted for this issuer and app client, in this token use.
    Served {
        issuer: String,
        client_id: String,
        token_use: &'static str,
    },
    /// The authorizer could not accept the presented token at all.
    Unauthorized,
}

/// One request to the deployed stage: the path within it, the value presented at the
/// authorizer's identity source (`None` to present nothing at all), and what the gate owes it.
struct Row {
    case: &'static str,
    path: &'static str,
    presented: Option<String>,
    expected: Expected,
}

/// One route of the api under test: its key, the integration it targets and the authorizer in
/// front of it.
struct Route<'a> {
    path: &'a str,
    integration_id: &'a str,
    authorizer_id: &'a str,
}

/// Poll the published address until the auto-deploying stage first serves the request — the
/// readiness a client of a freshly created stage waits for. The wait is on that CONDITION: a
/// request the stage does not serve yet is answered `404`, and anything else is the gate's own
/// verdict and ends the wait.
async fn wait_until_served(
    router: &MultiServiceRouter,
    authority: &str,
    path: &str,
    presented: &str,
) -> Answer {
    let deadline = Instant::now() + READY_BUDGET;
    loop {
        let answered = invoke(router, authority, path, Some(presented)).await;
        if answered.status != 404 {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "the published address never served {authority}{path}, last answer {}",
            answered.summary()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The whole rule end to end: two pools with their app clients, the two functions, the api with
/// one authorizer per `Audience` shape and per pool, the `$default` auto-deploying stage, and
/// the table of requests a client builds from the address the api published.
#[tokio::test]
async fn an_identity_tokens_audience_decides_the_jwt_gated_lambda_route() {
    let router = router();
    execution_role(&router).await;
    let served_function = "apigwv2-id-token-claims-echo";
    let spared_function = "apigwv2-id-token-spared";
    let (served_arn, spared_arn) = given_functions(
        &router,
        "apigwv2-id-token-audience",
        served_function,
        spared_function,
    )
    .await;

    // The pool the api's first two authorizers name, with TWO app clients: one each for the
    // audience entry a row matches and the client a row's token is minted for but no entry of
    // the addressed authorizer lists.
    let home = pool_with_signed_in_user(
        &router,
        "id-token-home-pool",
        &["audience-client", "other-client"],
        "home-user",
    )
    .await;
    // A SECOND pool, with its own authorizer on its own route: its tokens name an issuer the
    // home authorizers do not record, and the home pool's tokens name one IT does not record.
    let second = pool_with_signed_in_user(
        &router,
        "id-token-second-pool",
        &["second-pool-client", "second-pool-other-client"],
        "second-pool-user",
    )
    .await;

    let api = control(
        &router,
        "CreateApi",
        "POST",
        "/v2/apis",
        Some(json!({ "name": "id-token-audience-api", "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api_id = as_str("CreateApi", &api, "apiId");
    let announced = as_str("CreateApi", &api, "apiEndpoint");

    // One authorizer per `Audience` shape the rule generalises over: a single entry, two
    // entries (the matching one in either position), and a second pool's issuer altogether.
    let authorizers: Vec<(&str, &str, Vec<String>)> = vec![
        (
            "one-entry",
            &home.issuer,
            vec![home.clients[0].client_id.clone()],
        ),
        (
            "two-entries",
            &home.issuer,
            vec![
                home.clients[1].client_id.clone(),
                home.clients[0].client_id.clone(),
            ],
        ),
        (
            "second-pool",
            &second.issuer,
            vec![second.clients[0].client_id.clone()],
        ),
    ];
    let mut authorizer_ids = Vec::new();
    for (name, issuer, audience) in &authorizers {
        let created = control(
            &router,
            &format!("CreateAuthorizer({name})"),
            "POST",
            &format!("/v2/apis/{api_id}/authorizers"),
            Some(json!({
                "name": name,
                "authorizerType": "JWT",
                "identitySource": [IDENTITY_SOURCE],
                "jwtConfiguration": { "issuer": issuer, "audience": audience }
            })),
            201,
        )
        .await;
        let authorizer_id = as_str("CreateAuthorizer", &created, "authorizerId");
        // The configuration a later read reports is the one the create persisted, so the rows
        // below are decided by stored records rather than by anything the create held in hand.
        let read = control(
            &router,
            &format!("GetAuthorizer({name})"),
            "GET",
            &format!("/v2/apis/{api_id}/authorizers/{authorizer_id}"),
            None,
            200,
        )
        .await;
        let configured: Vec<String> = read["jwtConfiguration"]["audience"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            &configured, audience,
            "GetAuthorizer({name}) must report the Audience the create persisted, got {read}"
        );
        authorizer_ids.push(authorizer_id);
    }

    let mut integration_ids = Vec::new();
    for arn in [&served_arn, &spared_arn] {
        let integration = control(
            &router,
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

    let routes = [
        Route {
            path: ONE_ENTRY_PATH,
            integration_id: served_integration,
            authorizer_id: &authorizer_ids[0],
        },
        Route {
            path: TWO_ENTRY_PATH,
            integration_id: served_integration,
            authorizer_id: &authorizer_ids[1],
        },
        Route {
            path: SECOND_POOL_PATH,
            integration_id: served_integration,
            authorizer_id: &authorizer_ids[2],
        },
        Route {
            path: GUARDED_PATH,
            integration_id: spared_integration,
            authorizer_id: &authorizer_ids[0],
        },
        Route {
            path: SECOND_POOL_GUARDED_PATH,
            integration_id: spared_integration,
            authorizer_id: &authorizer_ids[2],
        },
    ];
    for route in &routes {
        control(
            &router,
            &format!("CreateRoute(GET {})", route.path),
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": format!("GET {}", route.path),
                "target": format!("integrations/{}", route.integration_id),
                "authorizationType": "JWT",
                "authorizerId": route.authorizer_id
            })),
            201,
        )
        .await;
    }

    let stage = control(
        &router,
        "CreateStage",
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": ROOT_STAGE, "autoDeploy": true })),
        201,
    )
    .await;
    assert!(
        stage.get("deploymentId").and_then(Value::as_str).is_some(),
        "an auto-deploying stage must carry a deployment, got {stage}"
    );

    let (authority, base_path) = announced_parts(&announced);
    // The home pool's first client is the only entry of the first authorizer's `Audience` and
    // the second entry of the two-entry one; its second client is listed by the two-entry
    // authorizer alone.
    let home_first = &home.clients[0];
    let home_second = &home.clients[1];
    let second_pool_listed = &second.clients[0];
    let second_pool_other = &second.clients[1];
    let bearer = |token: &str| Some(format!("Bearer {token}"));
    let rows = [
        Row {
            case: "the identity token of the authorizer's only Audience entry",
            path: ONE_ENTRY_PATH,
            presented: bearer(&home_first.id_token),
            expected: Expected::Served {
                issuer: home.issuer.clone(),
                client_id: home_first.client_id.clone(),
                token_use: "id",
            },
        },
        Row {
            case: "the same identity token presented bare, with no scheme prefix",
            path: ONE_ENTRY_PATH,
            presented: Some(home_first.id_token.clone()),
            expected: Expected::Served {
                issuer: home.issuer.clone(),
                client_id: home_first.client_id.clone(),
                token_use: "id",
            },
        },
        Row {
            case: "the access token of that same client, whose client_id carries the audience",
            path: ONE_ENTRY_PATH,
            presented: bearer(&home_first.access_token),
            expected: Expected::Served {
                issuer: home.issuer.clone(),
                client_id: home_first.client_id.clone(),
                token_use: "access",
            },
        },
        Row {
            case: "an identity token matching the FIRST entry of a two-entry Audience",
            path: TWO_ENTRY_PATH,
            presented: bearer(&home_second.id_token),
            expected: Expected::Served {
                issuer: home.issuer.clone(),
                client_id: home_second.client_id.clone(),
                token_use: "id",
            },
        },
        Row {
            case: "an identity token matching the SECOND entry of a two-entry Audience",
            path: TWO_ENTRY_PATH,
            presented: bearer(&home_first.id_token),
            expected: Expected::Served {
                issuer: home.issuer.clone(),
                client_id: home_first.client_id.clone(),
                token_use: "id",
            },
        },
        Row {
            case: "the second pool's own client at the route its own authorizer gates",
            path: SECOND_POOL_PATH,
            presented: bearer(&second_pool_listed.id_token),
            expected: Expected::Served {
                issuer: second.issuer.clone(),
                client_id: second_pool_listed.client_id.clone(),
                token_use: "id",
            },
        },
        Row {
            case: "an identity token of a second client of the SAME pool, listed by no entry",
            path: GUARDED_PATH,
            presented: bearer(&home_second.id_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "an identity token of ANOTHER user pool: an issuer this authorizer never named",
            path: GUARDED_PATH,
            presented: bearer(&second_pool_listed.id_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "the access token of that other pool's client",
            path: GUARDED_PATH,
            presented: bearer(&second_pool_listed.access_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "the home pool's listed client at the SECOND pool's authorizer",
            path: SECOND_POOL_GUARDED_PATH,
            presented: bearer(&home_first.id_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "a second client of the second pool, listed by no entry of its authorizer",
            path: SECOND_POOL_GUARDED_PATH,
            presented: bearer(&second_pool_other.id_token),
            expected: Expected::Unauthorized,
        },
        Row {
            case: "nothing presented at the authorizer's identity source",
            path: GUARDED_PATH,
            presented: None,
            expected: Expected::Unauthorized,
        },
    ];

    // The stage was created an instant ago, so the first request waits for it to serve at all —
    // the readiness the recorded client sequence polls for — and that answer is the first row's
    // own.
    let first = &rows[0];
    let ready = wait_until_served(
        &router,
        &authority,
        &format!("{base_path}{}", first.path),
        first
            .presented
            .as_deref()
            .expect("the first row presents a token"),
    )
    .await;
    assert_answer(first, &ready);

    // The refused rows run before the remaining served ones, so the spared function's log group
    // has every chance to hold a record by the time the served rows' own record is waited for
    // below.
    let (refused, served): (Vec<&Row>, Vec<&Row>) = rows[1..]
        .iter()
        .partition(|row| matches!(row.expected, Expected::Unauthorized));
    for row in refused.into_iter().chain(served) {
        let answered = invoke(
            &router,
            &authority,
            &format!("{base_path}{}", row.path),
            row.presented.as_deref(),
        )
        .await;
        assert_answer(row, &answered);
    }

    // The served rows' function RAN: its own log group holds the closing frame AWS Lambda writes
    // for every run it finishes. Waiting for that record is also the sync point the assertion
    // below needs — a refused row's invocation, had one been made, would have been recorded
    // before it.
    let served_group = log_group(served_function);
    let held = messages_until_condition(&router, &served_group, |held| {
        held.iter().any(|line| line.starts_with(REPORT_PREFIX))
    })
    .await;
    assert!(
        held.iter().any(|line| line.starts_with(REPORT_PREFIX)),
        "the admitted requests' function must have logged a run in {served_group:?}, held \
         {held:?}"
    );

    // And the function behind every REFUSED row ran not once: a refusal never reaches the
    // route's integration, so nothing was ever logged for it.
    let spared_records = messages_in(&router, &log_group(spared_function)).await;
    assert!(
        spared_records.is_empty(),
        "a refused request must not reach the route's integration, but the function behind the \
         guarded routes logged {spared_records:?}"
    );
}

/// Assert one row's answer: the function's own answer for an admitted request, carrying the
/// claims of the token the gate accepted, and the authorizer's refusal for every other.
fn assert_answer(row: &Row, answered: &Answer) {
    let case = row.case;
    assert_ne!(
        answered.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case {case:?}: the published invoke address must be one this emulator serves, got {}",
        answered.summary()
    );
    match &row.expected {
        Expected::Served {
            issuer,
            client_id,
            token_use,
        } => {
            assert_eq!(
                answered.status,
                SERVED,
                "case {case:?}: an admitted request must be answered by the route's own \
                 function, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.ran_header.as_deref(),
                Some(RAN_MARKER),
                "case {case:?}: the answer must carry the header the function itself set, got {}",
                answered.summary()
            );
            let reported = answered.json();
            let claim = |name: &str| {
                reported
                    .get(name)
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            let reached = claim("path").unwrap_or_default();
            assert!(
                reached.ends_with(row.path),
                "case {case:?}: the function must have been invoked for {}, got {}",
                row.path,
                answered.summary()
            );
            assert_eq!(
                claim("iss").as_deref(),
                Some(issuer.as_str()),
                "case {case:?}: the event must carry the accepted token's issuer {issuer:?}, \
                 got {}",
                answered.summary()
            );
            assert_eq!(
                claim("token_use").as_deref(),
                Some(*token_use),
                "case {case:?}: the event must carry the token use {token_use:?} this row \
                 presented, got {}",
                answered.summary()
            );
            // The claim that carried the audience: `aud` for an identity token, `client_id`
            // for an access token, which carries no `aud` at all.
            let audience_claim = match *token_use {
                "access" => "client_id",
                _ => "aud",
            };
            assert_eq!(
                claim(audience_claim).as_deref(),
                Some(client_id.as_str()),
                "case {case:?}: the event must carry {audience_claim:?} {client_id:?}, the \
                 Audience entry the gate matched, got {}",
                answered.summary()
            );
        }
        Expected::Unauthorized => {
            assert_eq!(
                answered.status,
                UNAUTHORIZED,
                "case {case:?}: expected HTTP {UNAUTHORIZED}, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.message(),
                UNAUTHORIZED_MESSAGE,
                "case {case:?}: expected the {UNAUTHORIZED_MESSAGE:?} body, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.ran_header, None,
                "case {case:?}: a refused request must not be answered by a function at all, \
                 got {}",
                answered.summary()
            );
        }
    }
}
