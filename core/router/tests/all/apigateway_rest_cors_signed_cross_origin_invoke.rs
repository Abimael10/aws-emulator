// SPDX-License-Identifier: CC0-1.0

//! The SIGNED cross-origin request a browser-based client of an IAM-authorized REST api makes,
//! driven end to end through the whole assembled router against an emulator that VERIFIES
//! request signatures.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A deployed REST stage reads a request's `Origin` as no authorization input. For a method
//! > whose `authorizationType` is `AWS_IAM` the one authorization input is the SigV4 signature:
//! > where it authenticates, the request is answered by the METHOD'S OWN INTEGRATION — which
//! > receives the `Origin` like every other header (an `AWS_PROXY` event reports it under
//! > `headers.origin`) and whose own `Access-Control-*` headers are what the browser reads,
//! > enforcement being the browser's — whichever origin the page that sent it was loaded from,
//! > including one the api's own `Access-Control-Allow-Origin` declaration does not name. Where
//! > the request carries no credential at all the stage refuses it `403 {"message":"Missing
//! > Authentication Token"}` with no `Access-Control-*` header, and where its signature does not
//! > authenticate it is refused `403` with no integration performed. The gateway answers the
//! > `OPTIONS` preflight itself, from the api's own records: the static values the `MOCK`
//! > method's integration response maps, whoever asked.
//!
//! The rows span what that rule generalises over: the origin the page was loaded from (the one
//! the api declares, another site's, and none at all — the client every other suite drives),
//! whether the request is signed with the keys this emulator published, whether the signature is
//! the one those keys produce, the verb (the one the declaration names and the preflight), and
//! the resource (the one whose records declare a CORS contract and one whose declare none).
//!
//! Only the assembled router can pin this: the credential store and the SigV4 verifier are the
//! emulator's, the stage, its records and its `Origin` handling are API Gateway's, the handler
//! behind the method is AWS Lambda's, IAM holds the role Lambda runs it as, and CloudFormation
//! archives the inline handler into the deployment package Lambda runs.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling `apigateway_iam_authenticated_invoke` suite does — on a MULTI-THREAD
//! runtime, because a cross-origin request is served from a seam the router hands no request
//! context, which drives the serving as a task of the runtime it is answered on.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for, and the Region the api's own invoke host and the
/// integration ARN name.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The signing name an invoke of a deployed stage is signed under: the `execute-api` service AWS
/// publishes the invoke endpoint and the `execute-api:Invoke` action under.
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// The resource whose records declare a CORS contract — a `MOCK` `OPTIONS` method of the shape
/// AWS's own "enable CORS" deploys, beside the `AWS_IAM` `GET` the page calls — and the one
/// whose records declare none at all.
const DECLARED_PATH: &str = "items";
const UNDECLARED_PATH: &str = "private";

/// The origin the page under test was loaded from, which the api's own declaration names.
const PAGE_ORIGIN: &str = "https://app.example.com";

/// Another site's origin, which that declaration does not name.
const OTHER_ORIGIN: &str = "https://evil.example";

/// The methods the declaration names, as a developer's own "enable CORS" writes them.
const DECLARED_METHODS: &str = "GET,OPTIONS";

/// The headers it names.
const DECLARED_HEADERS: &str = "authorization,x-amz-date";

/// The function both proxied methods are integrated with, the stack that declares it, and the
/// role Lambda runs it as. The handler makes no call of its own, so the role carries the trust
/// policy alone.
const FUNCTION: &str = "cors-signed-proxy-handler";
const STACK: &str = "cors-signed-proxy-stack";
const ROLE_NAME: &str = "cors-signed-proxy-exec";

/// How long one invocation is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long to wait between polls of a clock-owed transition, and how many polls to make: a
/// deadline generous enough that a loaded machine running the whole workspace's tests at once
/// still reaches the condition, because what each wait is for is the CONDITION and never a span.
const POLL: Duration = Duration::from_millis(100);
const POLLS: usize = 1_200;

/// The API version each awsQuery call carries.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// What the handler puts in its body and in a header of its own, so an answer that came from it
/// is told from one the api's records rendered.
const HANDLER_MARKER: &str = "sim2real-browser-cors";

/// The header the handler asks the browser to expose, which is a header only the handler sets.
const EXPOSED_HEADER: &str = "x-app-marker";

/// What the handler reports as the origin it saw when the event carried none.
const NO_ORIGIN_SEEN: &str = "none";

/// The sentence an `AWS_IAM` method reached with no credential at all is answered with.
const MISSING_TOKEN: &str = "Missing Authentication Token";

/// The handler behind both proxied methods: it reports the `Origin` THE EVENT CARRIED and sets
/// its own `Access-Control-*` headers from it, which is what a Lambda proxy handler of a
/// CORS-configured api does. The api's records declare none of those headers for this method, so
/// an answer carrying them can only have come from the handler, and one naming the request's own
/// origin only from an event that carried it.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    headers = event.get("headers") or {}
    seen = headers.get("origin") or headers.get("Origin")
    return {
        "statusCode": 200,
        "headers": {
            "Content-Type": "application/json",
            "Access-Control-Allow-Origin": seen or "none",
            "Access-Control-Expose-Headers": "x-app-marker",
            "x-app-marker": "sim2real-browser-cors",
        },
        "body": json.dumps({"marker": "sim2real-browser-cors", "seen": {"origin": seen}}),
    }
"#;

/// One answered request: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answered {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The `message` member a gateway response carries, or `None` for a body that is not one.
    fn message(&self) -> Option<String> {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn summary(&self) -> String {
        format!("{} {:?}", self.status, self.text())
    }
}

/// Dispatch one request through the router.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Vec<u8>>) -> Answered {
    let (parts, body) = request.into_parts();
    let response = router
        .dispatch(http::Request::from_parts(parts, Bytes::from(body)))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One request to `host` carrying `headers` and `body`.
fn built(
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> http::Request<Vec<u8>> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(body).expect("the test request is well formed")
}

/// One API Gateway REST control-plane call, signed with the published keys, which must answer
/// `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    keys: &Keys,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let answered = dispatch(
        router,
        signed(
            built(
                method,
                &host,
                path,
                &[("content-type", "application/json")],
                body.map(|body| body.to_string().into_bytes())
                    .unwrap_or_default(),
            ),
            "apigateway",
            &keys.key_id,
            &keys.secret,
        ),
    )
    .await;
    assert_eq!(
        answered.status,
        expected,
        "setup {what}: must answer HTTP {expected}, got {}",
        answered.summary()
    );
    answered.json()
}

/// One AWS Lambda call over its `restJson1` wire, signed with the published keys.
async fn lambda(
    router: &MultiServiceRouter,
    keys: &Keys,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let host = format!("lambda.{REGION}.amazonaws.com");
    dispatch(
        router,
        signed(
            built(
                method,
                &host,
                path,
                &[("content-type", "application/json")],
                body.map(|body| body.to_string().into_bytes())
                    .unwrap_or_default(),
            ),
            "lambda",
            &keys.key_id,
            &keys.secret,
        ),
    )
    .await
}

/// One awsQuery call to `signing_name`, signed with the published keys, which must answer 200;
/// answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    keys: &Keys,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        signed(
            built(
                "POST",
                host,
                "/",
                &[("content-type", "application/x-www-form-urlencoded")],
                form.into_bytes(),
            ),
            signing_name,
            &keys.key_id,
            &keys.secret,
        ),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup {signing_name} {action}: must answer HTTP 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// A required string member of a JSON object.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what} must carry a non-empty {name:?}, got {body}"))
}

/// Deploy the stack that declares the function: the inline source is archived into the
/// deployment package by CloudFormation, which is how a handler written here comes to be one
/// Lambda can run. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter, keys: &Keys, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Echo": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string();
    query(
        router,
        keys,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        CLOUDFORMATION_VERSION,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    // The stack's resources are their own services' to create, and the create completes from
    // the instant those services report them ready: look again until the stack says so rather
    // than sleeping a span on their behalf. Read with this suite's own SIGNED call, because
    // this emulator verifies every request's signature.
    let mut reached = String::new();
    let mut described = String::new();
    for _ in 0..POLLS {
        described = query(
            router,
            keys,
            "cloudformation",
            &format!("cloudformation.{REGION}.amazonaws.com"),
            CLOUDFORMATION_VERSION,
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        reached = element(&described, "StackStatus");
        if reached == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup CreateStack: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );

    // The invoke the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
    let mut configuration = Value::Null;
    for _ in 0..POLLS {
        configuration = lambda(
            router,
            keys,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup GetFunctionConfiguration: expected Lambda to report {FUNCTION:?} Active, got \
         {configuration}"
    );
    member("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Put one `AWS_PROXY` method of `verb` on `resource`, with `authorization_type`, declaring no
/// response of its own: the handler behind it is what sets the headers its answer carries.
async fn proxy_method(
    router: &MultiServiceRouter,
    keys: &Keys,
    api: &str,
    resource: &str,
    verb: &str,
    authorization_type: &str,
    function_arn: &str,
) {
    let method = format!("/restapis/{api}/resources/{resource}/methods/{verb}");
    apigw(
        router,
        keys,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": authorization_type })),
        201,
    )
    .await;
    apigw(
        router,
        keys,
        "PutIntegration",
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

/// The `MOCK` `OPTIONS` method a browser's preflight reaches, carrying the resource's CORS
/// declaration: the origin its answers are for and the methods and headers it names, each mapped
/// by the integration response and declared by the method response of the same status.
async fn declared_preflight(router: &MultiServiceRouter, keys: &Keys, api: &str, resource: &str) {
    let method = format!("/restapis/{api}/resources/{resource}/methods/OPTIONS");
    apigw(
        router,
        keys,
        "PutMethod(OPTIONS)",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        keys,
        "PutIntegration(OPTIONS)",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        })),
        201,
    )
    .await;
    apigw(
        router,
        keys,
        "PutMethodResponse(OPTIONS)",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": false,
                "method.response.header.Access-Control-Allow-Methods": false,
                "method.response.header.Access-Control-Allow-Headers": false,
            },
        })),
        201,
    )
    .await;
    apigw(
        router,
        keys,
        "PutIntegrationResponse(OPTIONS)",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "responseTemplates": { "application/json": "" },
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": format!("'{PAGE_ORIGIN}'"),
                "method.response.header.Access-Control-Allow-Methods":
                    format!("'{DECLARED_METHODS}'"),
                "method.response.header.Access-Control-Allow-Headers":
                    format!("'{DECLARED_HEADERS}'"),
            },
        })),
        201,
    )
    .await;
}

/// How a row's request presents a credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signer {
    /// Signed with the keys this emulator published, over the request as it is sent.
    PublishedKeys,
    /// Signed with the right key id and another secret, so the signature is one no secret of
    /// this emulator produces.
    AnotherSecret,
    /// No credential at all.
    Nothing,
}

/// What the api owes one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// The method's own integration answered: the handler's 200, the origin the event carried,
    /// and the `Access-Control-*` headers the handler itself set.
    ByTheHandler,
    /// The api's own records answered the preflight: 200, an empty body and the declared values.
    ByTheDeclaration,
    /// The stage refused it itself, through the api's own gateway response: `403` carrying this
    /// sentence under `message`, with no `Access-Control-*` header and no answer of the
    /// handler's.
    RefusedByTheStage(&'static str),
    /// Nothing of the api answered it at all.
    NotServed,
}

/// One request a browser (or, where it names no origin, an SDK client) sends to the stage.
struct Row {
    case: &'static str,
    verb: &'static str,
    path: &'static str,
    /// The origin the page was loaded from, or `None` for a client that sets no browser header.
    origin: Option<&'static str>,
    signer: Signer,
    answer: Answer,
}

/// The request one row sends: built, then signed as the row says.
fn request_of(row: &Row, api: &str, keys: &Keys) -> http::Request<Vec<u8>> {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(origin) = row.origin {
        // The two browser-set headers a cross-site request carries, which is what the front
        // door reads a request a browser sent from another site by.
        headers.push(("origin", origin));
        headers.push(("sec-fetch-site", "cross-site"));
    }
    let built = built(
        row.verb,
        &host,
        &format!("/{STAGE}/{}", row.path),
        &headers,
        Vec::new(),
    );
    match row.signer {
        Signer::Nothing => built,
        Signer::PublishedKeys => signed(built, INVOKE_SIGNING_NAME, &keys.key_id, &keys.secret),
        Signer::AnotherSecret => signed(
            built,
            INVOKE_SIGNING_NAME,
            &keys.key_id,
            "0123456789abcdef0123456789abcdef01234567",
        ),
    }
}

/// The keys this emulator published, which every call of this suite signs with.
struct Keys {
    key_id: String,
    secret: String,
}

/// A SigV4 `Authorization` over `request` for `signing_name`, with the keys this emulator
/// published — which is what every call of this suite carries, because the emulator it drives
/// verifies request signatures from the first call on, as one started with authentication on
/// does.
fn signed(
    request: http::Request<Vec<u8>>,
    signing_name: &str,
    key_id: &str,
    secret: &str,
) -> http::Request<Vec<u8>> {
    let mut request = request;
    let identity = Credentials::new(key_id, secret, None, None, "test").into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(REGION)
        .name(signing_name)
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .expect("the signing parameters are complete")
        .into();
    let (method, uri) = (request.method().to_string(), request.uri().to_string());
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().expect("a text header").to_string(),
            )
        })
        .collect();
    let signable = SignableRequest::new(
        &method,
        &uri,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(request.body()),
    )
    .expect("the request is signable");
    let (instructions, _) = sign(signable, &params)
        .expect("the request signs")
        .into_parts();
    instructions.apply_to_request_http1x(&mut request);
    request
}

#[test]
fn a_signed_cross_origin_invoke_is_answered_by_the_methods_own_integration() {
    std::thread::Builder::new()
        .name("apigateway-rest-cors-signed-cross-origin-invoke".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // MULTI-THREAD, because a cross-origin request is served from a seam the router
            // hands no request context: the serving is driven as a task of this runtime while
            // the seam waits for it, which a current-thread runtime cannot do.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a multi-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);

                // This emulator VERIFIES request signatures from its first call on, as one
                // started with authentication on does — every call below, the setup's included,
                // is signed with the keys it publishes. The rule under test is about what the
                // SIGNATURE on a cross-origin request earns it, so an emulator that verifies
                // none could not observe it.
                router.set_request_authentication(true);
                let published = ::emulator_server::SigV4Authenticator::minted_credentials()
                    .expect("turning authentication on publishes this process's keys");
                let keys = Keys {
                    key_id: published.access_key_id.clone(),
                    secret: published.secret_access_key.clone(),
                };

                // SETUP: the function the methods proxy to, the role Lambda runs it as, the
                // grant that admits the invocation, and the api whose records declare the CORS
                // contract.
                let role = query(
                    &router,
                    &keys,
                    "iam",
                    "iam.amazonaws.com",
                    IAM_VERSION,
                    "CreateRole",
                    &[
                        ("RoleName", ROLE_NAME),
                        (
                            "AssumeRolePolicyDocument",
                            &json!({
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Principal": { "Service": "lambda.amazonaws.com" },
                                    "Action": "sts:AssumeRole",
                                }],
                            })
                            .to_string(),
                        ),
                    ],
                )
                .await;
                let role_arn = element(&role, "Arn");
                let function_arn = deployed_function(&router, &keys, &role_arn).await;
                let granted = lambda(
                    &router,
                    &keys,
                    "POST",
                    &format!("/2015-03-31/functions/{FUNCTION}/policy"),
                    Some(json!({
                        "StatementId": "apigw-cors-signed",
                        "Action": "lambda:InvokeFunction",
                        "Principal": "apigateway.amazonaws.com",
                    })),
                )
                .await;
                assert_eq!(
                    granted.status,
                    201,
                    "setup AddPermission: must answer HTTP 201, got {}",
                    granted.summary()
                );

                let created = apigw(
                    &router,
                    &keys,
                    "CreateRestApi",
                    "POST",
                    "/restapis",
                    Some(json!({ "name": "cors-signed-cross-origin" })),
                    201,
                )
                .await;
                let api = member("CreateRestApi", &created, "id");
                let root = member("CreateRestApi", &created, "rootResourceId");
                for path_part in [DECLARED_PATH, UNDECLARED_PATH] {
                    let resource = apigw(
                        &router,
                        &keys,
                        "CreateResource",
                        "POST",
                        &format!("/restapis/{api}/resources/{root}"),
                        Some(json!({ "pathPart": path_part })),
                        201,
                    )
                    .await;
                    let resource = member("CreateResource", &resource, "id");
                    proxy_method(
                        &router,
                        &keys,
                        &api,
                        &resource,
                        "GET",
                        "AWS_IAM",
                        &function_arn,
                    )
                    .await;
                    if path_part == DECLARED_PATH {
                        declared_preflight(&router, &keys, &api, &resource).await;
                    }
                }
                apigw(
                    &router,
                    &keys,
                    "CreateDeployment",
                    "POST",
                    &format!("/restapis/{api}/deployments"),
                    Some(json!({ "stageName": STAGE })),
                    201,
                )
                .await;

                let rows = [
                    Row {
                        case: "a signed cross-origin GET from the origin the api declares",
                        verb: "GET",
                        path: DECLARED_PATH,
                        origin: Some(PAGE_ORIGIN),
                        signer: Signer::PublishedKeys,
                        answer: Answer::ByTheHandler,
                    },
                    Row {
                        case: "the same request from a client that sets no browser header",
                        verb: "GET",
                        path: DECLARED_PATH,
                        origin: None,
                        signer: Signer::PublishedKeys,
                        answer: Answer::ByTheHandler,
                    },
                    Row {
                        case: "a signed cross-origin GET from a site the api declares nothing \
                               for, which the Origin cannot refuse",
                        verb: "GET",
                        path: DECLARED_PATH,
                        origin: Some(OTHER_ORIGIN),
                        signer: Signer::PublishedKeys,
                        answer: Answer::ByTheHandler,
                    },
                    Row {
                        case: "the same cross-origin GET carrying no credential at all",
                        verb: "GET",
                        path: DECLARED_PATH,
                        origin: Some(PAGE_ORIGIN),
                        signer: Signer::Nothing,
                        answer: Answer::RefusedByTheStage(MISSING_TOKEN),
                    },
                    Row {
                        case: "the same unsigned GET from a client that sets no browser header",
                        verb: "GET",
                        path: DECLARED_PATH,
                        origin: None,
                        signer: Signer::Nothing,
                        answer: Answer::RefusedByTheStage(MISSING_TOKEN),
                    },
                    Row {
                        case: "the preflight the browser sends before that GET",
                        verb: "OPTIONS",
                        path: DECLARED_PATH,
                        origin: Some(PAGE_ORIGIN),
                        signer: Signer::Nothing,
                        answer: Answer::ByTheDeclaration,
                    },
                    Row {
                        case: "the preflight from the site the api declares nothing for, \
                               answered the declared value whoever asked",
                        verb: "OPTIONS",
                        path: DECLARED_PATH,
                        origin: Some(OTHER_ORIGIN),
                        signer: Signer::Nothing,
                        answer: Answer::ByTheDeclaration,
                    },
                    Row {
                        case: "a signed cross-origin GET to a resource whose records declare no \
                               contract at all, which the signature decides anyway",
                        verb: "GET",
                        path: UNDECLARED_PATH,
                        origin: Some(PAGE_ORIGIN),
                        signer: Signer::PublishedKeys,
                        answer: Answer::ByTheHandler,
                    },
                    Row {
                        case: "the same request to that resource carrying no credential, which \
                               nothing of the api admits",
                        verb: "GET",
                        path: UNDECLARED_PATH,
                        origin: Some(PAGE_ORIGIN),
                        signer: Signer::Nothing,
                        answer: Answer::NotServed,
                    },
                    Row {
                        case: "the same request to that resource with no browser header",
                        verb: "GET",
                        path: UNDECLARED_PATH,
                        origin: None,
                        signer: Signer::PublishedKeys,
                        answer: Answer::ByTheHandler,
                    },
                ];

                for row in &rows {
                    let case = row.case;
                    let answered =
                        dispatch(&router, request_of(row, &api, &keys)).await;
                    let allow_origin = answered.header("access-control-allow-origin");
                    match row.answer {
                        Answer::ByTheHandler => {
                            assert_eq!(
                                answered.status,
                                200,
                                "case {case:?}: expected the handler's own 200, got {}",
                                answered.summary()
                            );
                            let reported = answered.json();
                            assert_eq!(
                                reported.get("marker").and_then(Value::as_str),
                                Some(HANDLER_MARKER),
                                "case {case:?}: expected the handler's own body, got {}",
                                answered.summary()
                            );
                            let seen = reported
                                .get("seen")
                                .and_then(|seen| seen.get("origin"))
                                .and_then(Value::as_str);
                            assert_eq!(
                                seen,
                                row.origin,
                                "case {case:?}: expected the event to carry the request's own \
                                 Origin {:?}, got {seen:?}",
                                row.origin
                            );
                            assert_eq!(
                                allow_origin.as_deref(),
                                Some(row.origin.unwrap_or(NO_ORIGIN_SEEN)),
                                "case {case:?}: expected the handler's own \
                                 Access-Control-Allow-Origin, got {allow_origin:?}"
                            );
                            assert_eq!(
                                answered.header("access-control-expose-headers").as_deref(),
                                Some(EXPOSED_HEADER),
                                "case {case:?}: expected the handler's own \
                                 Access-Control-Expose-Headers, got {:?}",
                                answered.header("access-control-expose-headers")
                            );
                            assert_eq!(
                                answered.header(EXPOSED_HEADER).as_deref(),
                                Some(HANDLER_MARKER),
                                "case {case:?}: expected the header the handler set, got {:?}",
                                answered.header(EXPOSED_HEADER)
                            );
                        }
                        Answer::ByTheDeclaration => {
                            assert_eq!(
                                (answered.status, answered.text().as_str()),
                                (200, ""),
                                "case {case:?}: expected the declared preflight answer, got {}",
                                answered.summary()
                            );
                            for (name, expected) in [
                                ("access-control-allow-origin", PAGE_ORIGIN),
                                ("access-control-allow-methods", DECLARED_METHODS),
                                ("access-control-allow-headers", DECLARED_HEADERS),
                            ] {
                                let actual = answered.header(name);
                                assert_eq!(
                                    actual.as_deref(),
                                    Some(expected),
                                    "case {case:?}: expected the declared {name} {expected:?}, \
                                     got {actual:?}"
                                );
                            }
                        }
                        Answer::RefusedByTheStage(sentence) => {
                            assert_eq!(
                                answered.status,
                                403,
                                "case {case:?}: expected the stage's own 403, got {}",
                                answered.summary()
                            );
                            assert_eq!(
                                answered.message().as_deref(),
                                Some(sentence),
                                "case {case:?}: expected the sentence {sentence:?}, got {}",
                                answered.summary()
                            );
                            assert!(
                                !answered.text().contains(HANDLER_MARKER),
                                "case {case:?}: the integration must not have been performed, \
                                 got {}",
                                answered.summary()
                            );
                            assert_eq!(
                                allow_origin, None,
                                "case {case:?}: expected no Access-Control-Allow-Origin on the \
                                 stage's own refusal, got {allow_origin:?}"
                            );
                        }
                        Answer::NotServed => {
                            assert_eq!(
                                answered.status,
                                403,
                                "case {case:?}: expected the stage not to serve it, got {}",
                                answered.summary()
                            );
                            assert!(
                                !answered.text().contains(HANDLER_MARKER)
                                    && !answered.text().contains(MISSING_TOKEN),
                                "case {case:?}: expected no answer of the api's own, got {}",
                                answered.summary()
                            );
                            assert_eq!(
                                allow_origin, None,
                                "case {case:?}: expected no Access-Control-Allow-Origin, got \
                                 {allow_origin:?}"
                            );
                        }
                    }
                }

                // The BOUNDARY the rule's authentication half rests on: a signature no secret
                // of this emulator produces is refused with no integration performed, so what
                // the serving of a cross-origin request admits is a caller holding these keys
                // and not a page that merely set an Origin.
                let forged = Row {
                    case: "a cross-origin GET signed with another secret",
                    verb: "GET",
                    path: DECLARED_PATH,
                    origin: Some(PAGE_ORIGIN),
                    signer: Signer::AnotherSecret,
                    answer: Answer::NotServed,
                };
                let answered = dispatch(&router, request_of(&forged, &api, &keys)).await;
                assert_eq!(
                    answered.status,
                    403,
                    "case {:?}: expected 403, got {}",
                    forged.case,
                    answered.summary()
                );
                assert!(
                    !answered.text().contains(HANDLER_MARKER),
                    "case {:?}: the integration must not have been performed, got {}",
                    forged.case,
                    answered.summary()
                );
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
