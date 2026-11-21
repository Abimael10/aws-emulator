// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the BODY and the exception CLASS a deployed stage answers a call its
//! Lambda authorizer produced no decision for.
//!
//! THE RULE UNDER TEST, over a SET of such calls rather than one recorded request: a gateway
//! response's own default body is that response type's default template
//! (`{"message":$context.error.messageString}`) rendered for the refusal, and
//! `messageString` is the QUOTED value of `$context.error.message` — so a refusal class the
//! gateway sets no error message for renders the quoted value of nothing, which is JSON
//! `null`. A call whose `CUSTOM` authorizer produced no decision is such a class, whatever
//! left it undecided: its body is exactly `{"message":null}`, with the `null` a JSON literal
//! rather than a quoted sentence. A refusal class that DOES set a message keeps its own
//! sentence, so the same api answers an integration it cannot perform
//! `{"message":"Internal server error"}` and a token the authorizer refused its own `403`
//! sentence. The CLASS the refusal names in `x-amzn-errortype` splits the same way: a call no
//! authorizer decided is an `AuthorizerConfigurationException`, while a method's own
//! configuration is the generic server fault.
//!
//! Why it matters: `{"message":null}` is the one thing that tells a client an authorizer it
//! must fix from a backend that broke behind a call the gateway DID admit. Answering both with
//! the generic internal-error body makes the two indistinguishable, so alerting keyed on the
//! documented body misroutes.
//!
//! The rows span what the rule generalises over: WHAT left the call undecided (a function that
//! raised, a function that returned something that is not an authorizer output, an
//! `authorizerUri` that names no Lambda invocation path at all, a `uri` naming a function
//! Lambda holds no record of, a function whose own resource policy grants the authorizer no
//! invocation), WHICH authorizer type gates the method (`TOKEN`, which is handed the token,
//! and `REQUEST`, which is handed the whole request), and — in the second test — whether the
//! developer has CUSTOMIZED the gateway response's template, where the same unset message has
//! to render as JSON `null` wherever the template names it.
//!
//! The controls pin that the `null` is this class's own: an allowed call is served the
//! integration's body, a function raising the documented `Unauthorized` error earns the `401`
//! sentence, a returned `Deny` earns the `403` one, and a method whose INTEGRATION cannot be
//! performed earns the `Internal server error` sentence from the same stage.
//!
//! Only a suite spanning both services can pin this: the authorizer's function and the grant
//! on it are AWS Lambda's, and the api, the authorizer record, the method and the refusal are
//! API Gateway's. The harness is the sibling permission suite's
//! ([`crate::test_invoke_method_lambda_permission`]), reused rather than written again.

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::test_invoke_method_lambda_permission::{
    accepted, emulator, lambda, member, package, wait_until_active, REGION,
};

/// The stage both apis below are deployed to.
const STAGE: &str = "live";

/// The function every authorizer that HAS one names, and the role and handler a created
/// function declares. No IAM store is mounted here, so the role is a value the create carries
/// rather than a record this suite relies on.
const FUNCTION: &str = "authorizer-failure-gate";
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";
const HANDLER: &str = "lambda_function.lambda_handler";

/// The header every authorizer below declares as its identity source.
const IDENTITY_HEADER: &str = "authorization";

/// The tokens the function below answers each way for. They are its INPUTS, not values read
/// back from anything.
const FAILS: &str = "fails";
const MALFORMED: &str = "malformed";
const UNAUTHORIZED_TOKEN: &str = "unauthorized";
const DENIED: &str = "denied";
const ALLOWED: &str = "allowed";

/// The body a call no authorizer decided is answered with: the response type's own default
/// template rendered with no message set, so the `message` member is the JSON literal `null`.
const NO_DECISION: (u16, &str) = (500, r#"{"message":null}"#);

/// The status and body a method whose INTEGRATION cannot be performed earns — a class the
/// gateway DOES set a message for, which is what makes the row above a distinction a client
/// can read.
const UNSERVABLE_INTEGRATION: (u16, &str) = (500, r#"{"message":"Internal server error"}"#);

/// The status and body a call that authenticated nobody earns: the sentence API Gateway words
/// the refusal a `TOKEN` authorizer raises `Unauthorized` for.
const UNAUTHORIZED: (u16, &str) = (401, r#"{"message":"Unauthorized"}"#);

/// The sentence a call the returned policy explicitly DENIES is refused with, and its status.
const EXPLICIT_DENY: (u16, &str) = (
    403,
    "User is not authorized to access this resource with an explicit deny in an identity-based \
     policy",
);

/// The body the gated methods' `MOCK` integration answers an admitted call with: the
/// template's own fixed text, so a served call is never mistaken for a refusal.
const SERVED_BODY: &str = r#"{"served":true}"#;

/// The ONE authorizer function every row with a real function is decided by: it reads the
/// token out of whichever event its authorizer type hands it and chooses what to do from that
/// token alone, so each leg a client sees is this function's own answer.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    token = event.get("authorizationToken")
    if token is None:
        headers = event.get("headers") or {}
        token = headers.get("Authorization") or headers.get("authorization") or ""
    if token == "fails":
        raise Exception("the authorizer could not read the secret it needs")
    if token == "unauthorized":
        raise Exception("Unauthorized")
    if token == "malformed":
        return {"nothing": "here is an authorizer output"}
    effect = "Deny" if token == "denied" else "Allow"
    return {
        "principalId": "caller-" + token,
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

/// The `authorizerUri` of an `AWS` integration ARN naming an action other than the Lambda
/// function-invoking one, so it names no function to invoke at all: `Integration$uri` gives
/// the AWS form as `arn:aws:apigateway:{region}:{subdomain.service|service}:path|action/{path}`
/// and this one names the S3 subdomain's path form.
fn not_a_function_uri() -> String {
    format!("arn:aws:apigateway:{REGION}:s3:path/bucket/key")
}

/// The `authorizerUri` naming `function_arn`'s invocation path, the form
/// `Authorizer$authorizerUri` documents for a `TOKEN` or `REQUEST` authorizer.
fn invocation_uri(function_arn: &str) -> String {
    format!(
        "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/invocations"
    )
}

/// The ARN of a function this emulator holds no record of, which is what an authorizer naming
/// it cannot have invoked.
fn absent_function_arn() -> String {
    format!(
        "arn:aws:lambda:{REGION}:{account}:function:no-such-authorizer-function",
        account = engine::emulator_account_id(),
    )
}

/// The header API Gateway names the exception class of a refused request in, lowercased as
/// HTTP field names compare, and the classes the rows read from it: the authorizer's own for a
/// call it produced no decision for, the server fault for a method's own configuration, and
/// the one a request that authenticated nobody earns.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const AUTHORIZER_CONFIGURATION_EXCEPTION: &str = "AuthorizerConfigurationException";
const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";
const UNAUTHORIZED_EXCEPTION: &str = "UnauthorizedException";

/// One answered invoke: its status, its body verbatim and the exception class it named.
struct Answer {
    status: u16,
    body: String,
    class: Option<String>,
}

impl Answer {
    /// The body as JSON, `Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` — carrying `headers` and NO credential,
/// as a client of a custom-authorized method sends none.
async fn invoke(
    router: &MultiServiceRouter,
    api: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}/{STAGE}{path}"))
        .header("host", &host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(Bytes::new()).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).to_string(),
        class: parts
            .headers
            .get(ERROR_TYPE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    }
}

/// The same request, presenting `token` at the identity source every authorizer declares.
async fn invoke_as(router: &MultiServiceRouter, api: &str, path: &str, token: &str) -> Answer {
    invoke(router, api, path, &[(IDENTITY_HEADER, token)]).await
}

/// Create the authorizer function and answer the ARN Lambda holds it under.
async fn created_function(router: &MultiServiceRouter) -> String {
    let (status, function) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected HTTP 201, got {status} with {function}"
    );
    let arn = member("CreateFunction", &function, "FunctionArn");
    wait_until_active(router, FUNCTION).await;
    arn
}

/// Create one api and answer its id beside its root resource's id.
async fn created_api(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let api = accepted(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api, "id");
    let resources = accepted(
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
    (api, root)
}

/// One method gated by an authorizer: the resource path it sits on, the `Authorizer$type`, the
/// `authorizerUri` it names and whether the function's own resource policy grants that
/// authorizer an invocation.
struct Gated {
    path: &'static str,
    kind: &'static str,
    uri: String,
    granted: bool,
}

/// Put a `MOCK` integration answering [`SERVED_BODY`] on `GET` of `resource`, so an admitted
/// call is answered out of the api's own records and a refusal is told from it by the body.
async fn mock_integration(router: &MultiServiceRouter, api: &str, resource: &str) {
    accepted(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": r#"{"statusCode": 200}"# },
        })),
        201,
    )
    .await;
    accepted(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    accepted(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"),
        Some(json!({ "responseTemplates": { "application/json": SERVED_BODY } })),
        201,
    )
    .await;
}

/// Stand up one gated method on `api`: its authorizer, the grant on the function where the row
/// has one, the resource, the `CUSTOM` method and the `MOCK` integration behind it.
async fn gate(router: &MultiServiceRouter, api: &str, root: &str, gated: &Gated) {
    let part = gated.path.trim_start_matches('/');
    let authorizer = accepted(
        router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": format!("{part}-authorizer"),
            "type": gated.kind,
            "authorizerUri": gated.uri,
            "identitySource": format!("method.request.header.{IDENTITY_HEADER}"),
            // "If it equals 0, authorization caching is disabled": every row reaches the
            // function itself rather than a result another row left on the stage.
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer = member("CreateAuthorizer", &authorizer, "id");
    if gated.granted {
        // "Grants a principal permission to use a function" (`AddPermission`), scoped by
        // `SourceArn` to the AUTHORIZER whose function is invoked.
        let (status, granted) = lambda(
            router,
            "POST",
            &format!("/2015-03-31/functions/{FUNCTION}/policy"),
            Some(json!({
                "StatementId": format!("apigateway-invoke-{part}"),
                "Action": "lambda:InvokeFunction",
                "Principal": "apigateway.amazonaws.com",
                "SourceArn": format!(
                    "arn:aws:execute-api:{REGION}:{account}:{api}/authorizers/{authorizer}",
                    account = engine::emulator_account_id(),
                ),
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "setup AddPermission({part}): expected HTTP 201, got {status} with {granted}"
        );
    }
    let resource = accepted(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": part })),
        201,
    )
    .await;
    let resource = member("CreateResource", &resource, "id");
    accepted(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET"),
        Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer })),
        201,
    )
    .await;
    mock_integration(router, api, &resource).await;
}

/// Stand up the one route whose own INTEGRATION cannot be performed: an open method proxying
/// to a function Lambda holds no record of, which is the control the authorizer rows are read
/// against.
async fn unservable_route(router: &MultiServiceRouter, api: &str, root: &str, part: &str) {
    let resource = accepted(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": part })),
        201,
    )
    .await;
    let resource = member("CreateResource", &resource, "id");
    accepted(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    accepted(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            "httpMethod": "POST",
            "uri": invocation_uri(&absent_function_arn()),
        })),
        201,
    )
    .await;
}

/// Release a deployment of `api` to [`STAGE`].
async fn deploy(router: &MultiServiceRouter, api: &str) {
    accepted(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
}

/// One answered invoke: what the request presented and where, and the answer the rule owes.
struct Row {
    case: &'static str,
    path: &'static str,
    /// The identity the request presents, or `None` to present none at all.
    token: Option<&'static str>,
    status: u16,
    /// The body, verbatim, for a row whose body is fixed text; `None` for the row whose body
    /// is the integration's own.
    body: Option<&'static str>,
    /// The exception class the refusal names, or `None` for the row that is served.
    class: Option<&'static str>,
}

/// Assert one row's status, body and exception class.
async fn assert_row(router: &MultiServiceRouter, api: &str, row: &Row) {
    let answered = match row.token {
        Some(token) => invoke_as(router, api, row.path, token).await,
        None => invoke(router, api, row.path, &[]).await,
    };
    assert_eq!(
        answered.status, row.status,
        "case {:?}: expected HTTP {}, got {} with {:?}",
        row.case, row.status, answered.status, answered.body
    );
    let expected = row.body.unwrap_or(SERVED_BODY);
    assert_eq!(
        answered.body, expected,
        "case {:?}: expected the body {expected:?}, got {:?}",
        row.case, answered.body
    );
    assert_eq!(
        answered.class.as_deref(),
        row.class,
        "case {:?}: expected the class {:?}, got {:?}",
        row.case,
        row.class,
        answered.class
    );
}

#[tokio::test]
async fn every_call_its_authorizer_left_undecided_carries_the_null_message_body() {
    let router = emulator();
    let function_arn = created_function(&router).await;
    let (api, root) = created_api(&router, "authorizer-failure-body").await;

    let gates = [
        // The function itself decides these two, one per authorizer type.
        Gated {
            path: "/token-gate",
            kind: "TOKEN",
            uri: invocation_uri(&function_arn),
            granted: true,
        },
        Gated {
            path: "/request-gate",
            kind: "REQUEST",
            uri: invocation_uri(&function_arn),
            granted: true,
        },
        // And three the invocation never reaches a function's code for.
        Gated {
            path: "/ungranted",
            kind: "TOKEN",
            uri: invocation_uri(&function_arn),
            granted: false,
        },
        Gated {
            path: "/absent-function",
            kind: "TOKEN",
            uri: invocation_uri(&absent_function_arn()),
            granted: true,
        },
        Gated {
            path: "/not-a-function",
            kind: "REQUEST",
            uri: not_a_function_uri(),
            granted: true,
        },
    ];
    for gated in &gates {
        gate(&router, &api, &root, gated).await;
    }
    unservable_route(&router, &api, &root, "broken-backend").await;
    deploy(&router, &api).await;

    let rows: &[Row] = &[
        Row {
            case: "a TOKEN authorizer whose function raised",
            path: "/token-gate",
            token: Some(FAILS),
            status: NO_DECISION.0,
            body: Some(NO_DECISION.1),
            class: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        },
        Row {
            case: "a REQUEST authorizer whose function raised",
            path: "/request-gate",
            token: Some(FAILS),
            status: NO_DECISION.0,
            body: Some(NO_DECISION.1),
            class: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        },
        Row {
            case: "a function returning no authorizer output",
            path: "/token-gate",
            token: Some(MALFORMED),
            status: NO_DECISION.0,
            body: Some(NO_DECISION.1),
            class: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        },
        Row {
            case: "a REQUEST authorizer whose function returned no authorizer output",
            path: "/request-gate",
            token: Some(MALFORMED),
            status: NO_DECISION.0,
            body: Some(NO_DECISION.1),
            class: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        },
        Row {
            case: "an authorizer the function's resource policy grants no invocation of",
            path: "/ungranted",
            token: Some(ALLOWED),
            status: NO_DECISION.0,
            body: Some(NO_DECISION.1),
            class: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        },
        Row {
            case: "an authorizerUri naming a function Lambda holds no record of",
            path: "/absent-function",
            token: Some(ALLOWED),
            status: NO_DECISION.0,
            body: Some(NO_DECISION.1),
            class: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        },
        Row {
            case: "an authorizerUri naming no Lambda invocation path at all",
            path: "/not-a-function",
            token: Some(ALLOWED),
            status: NO_DECISION.0,
            body: Some(NO_DECISION.1),
            class: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        },
        // The controls: the classes that DO carry a sentence, from the same stage.
        Row {
            case: "a function raising the documented Unauthorized error",
            path: "/token-gate",
            token: Some(UNAUTHORIZED_TOKEN),
            status: UNAUTHORIZED.0,
            body: Some(UNAUTHORIZED.1),
            class: Some(UNAUTHORIZED_EXCEPTION),
        },
        Row {
            case: "no identity source at all, so the function is not invoked",
            path: "/token-gate",
            token: None,
            status: UNAUTHORIZED.0,
            body: Some(UNAUTHORIZED.1),
            class: Some(UNAUTHORIZED_EXCEPTION),
        },
        Row {
            case: "a method whose integration cannot be performed",
            path: "/broken-backend",
            token: None,
            status: UNSERVABLE_INTEGRATION.0,
            body: Some(UNSERVABLE_INTEGRATION.1),
            class: Some(INTERNAL_SERVER_ERROR_EXCEPTION),
        },
        Row {
            case: "an allowed call, served the integration's own body",
            path: "/token-gate",
            token: Some(ALLOWED),
            status: 200,
            body: None,
            class: None,
        },
    ];
    for row in rows {
        assert_row(&router, &api, row).await;
    }

    // The returned Deny carries IAM's own sentence under the member an authorization refusal
    // is worded in, so the `null` above is the undecided class's and not every refusal's.
    let denied = invoke_as(&router, &api, "/token-gate", DENIED).await;
    assert_eq!(
        denied.status,
        EXPLICIT_DENY.0,
        "case \"a returned explicit Deny\": expected HTTP {}, got {} with {:?}",
        EXPLICIT_DENY.0,
        denied.status,
        denied.body
    );
    assert_eq!(
        denied.json().get("Message").and_then(Value::as_str),
        Some(EXPLICIT_DENY.1),
        "case \"a returned explicit Deny\": expected the sentence {:?}, got {:?}",
        EXPLICIT_DENY.1,
        denied.body
    );
}

#[tokio::test]
async fn a_customized_authorizer_gateway_response_renders_the_unset_message_as_json_null() {
    // A developer's own template names the same values the default one does, so an unset
    // message has to reach it as JSON `null` wherever it is named — and the response type the
    // refusal is answered through is the one the customization applies to.
    let router = emulator();
    let function_arn = created_function(&router).await;
    let (api, root) = created_api(&router, "authorizer-failure-body-customized").await;
    gate(
        &router,
        &api,
        &root,
        &Gated {
            path: "/token-gate",
            kind: "TOKEN",
            uri: invocation_uri(&function_arn),
            granted: true,
        },
    )
    .await;
    for response_type in ["AUTHORIZER_FAILURE", "AUTHORIZER_CONFIGURATION_ERROR"] {
        accepted(
            &router,
            "PutGatewayResponse",
            "PUT",
            &format!("/restapis/{api}/gatewayresponses/{response_type}"),
            Some(json!({
                "responseTemplates": {
                    "application/json": "{\"m\":$context.error.messageString,\
                                          \"t\":\"$context.error.responseType\"}",
                },
            })),
            201,
        )
        .await;
    }
    deploy(&router, &api).await;

    let answered = invoke_as(&router, &api, "/token-gate", FAILS).await;
    let expected = r#"{"m":null,"t":"AUTHORIZER_FAILURE"}"#;
    assert_eq!(
        answered.status, 500,
        "case \"a customized AUTHORIZER_FAILURE response\": expected HTTP 500, got {} with {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.body, expected,
        "case \"a customized AUTHORIZER_FAILURE response\": expected {expected:?}, got {:?}",
        answered.body
    );
}
