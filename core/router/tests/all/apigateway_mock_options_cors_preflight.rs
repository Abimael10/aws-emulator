// SPDX-License-Identifier: CC0-1.0

//! The CORS contract of a deployed REST resource, driven end to end through the whole
//! assembled router: the `OPTIONS` preflight a browser sends before a cross-origin call, and
//! the real request that follows it into an `AWS_PROXY` Lambda integration.
//!
//! Four services and one engine, so this is a router suite: API Gateway resolves the stage and
//! answers the preflight from its own records, Lambda runs the function the real request's
//! integration invokes, IAM holds the role Lambda runs it as, and CloudFormation archives the
//! inline handler into the deployment package.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A deployed stage reads a request's `Origin` for NOTHING about which answer it serves. The
//! > `OPTIONS` preflight is answered by API Gateway itself from the resource's own records —
//! > the `MOCK` integration's request template selects the integration response, whose
//! > `method.response.header.<Name>` mappings are served as response headers, each carrying the
//! > static literal the mapping declares with its surrounding single quotes STRIPPED — and the
//! > real request that follows is answered by its own method's integration, performed as it is
//! > for any other caller, with `Origin` forwarded to the integration like every other header.
//!
//! The rows span what that rule generalises over: the verb (the preflight the `MOCK` `OPTIONS`
//! method answers, the real request the `AWS_PROXY` method answers), how the request reads as a
//! browser's from another site (an `Origin` alone, as an older browser's preflight carries it,
//! an `Origin` beside the browser's own `cross-site` classification, and no browser header at
//! all), and the addressing the client reaches the stage by (the api's own `execute-api` host,
//! and the path-style address this emulator announces on its own endpoint).
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling `apigateway_proxy_event_caller_identity` suite does; the runtime is a
//! MULTI-THREAD one, because serving a request a browser sent from another site is driven from
//! a synchronous seam that hands the work to the runtime.

use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, form_encode};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for, and the Region the integration ARN names.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, and the resource the CORS contract is configured on.
const STAGE: &str = "test";
const RESOURCE_PATH: &str = "items";

/// The origin the page under test was loaded from, which the `OPTIONS` method's integration
/// response declares.
const PAGE_ORIGIN: &str = "https://app.sim2real.example";

/// The function the real request's `AWS_PROXY` integration invokes, the stack that declares it
/// and the execution role Lambda runs it as.
const FUNCTION: &str = "apigw-cors-items";
const STACK: &str = "apigw-cors-items-stack";
const ROLE_NAME: &str = "apigw-cors-items-exec";

/// How long one invocation is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long to wait between looks at a transition another service owes, and how many looks to
/// make. Nothing here asserts on how long anything took: the wait is on the CONDITION.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 400;

/// The API version each awsQuery call carries.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// An access key id no identity registry of this emulator holds, which is what the setup calls
/// carry so the router can name the target service.
const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The handler the real request reaches: it answers a proxy response whose body reports the
/// `Origin` the event carried, so what is asserted is that the integration ran and was handed
/// the request's own header. The `1.0` proxy event spells a header name the way HTTP
/// capitalises a field name, so the handler reads `Origin`.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    headers = event.get("headers") or {}
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"origin": headers.get("Origin")}),
    }
"#;

/// The `Access-Control-*` headers the `OPTIONS` method's integration response maps, as the
/// header a browser reads and the LITERAL the mapping is stored with: each is "a static value
/// enclosed within a pair of single quotes" (`PutIntegrationResponse$responseParameters`), and
/// what a caller is served is that value with the quotes stripped.
///
/// The four names are the ones AWS's own "Enabling CORS for a REST API resource" flow declares,
/// and they span the value shapes a preflight carries: an origin, a verb list, a header list
/// and a bare number.
const DECLARED_PREFLIGHT_HEADERS: &[(&str, &str)] = &[
    ("access-control-allow-origin", PAGE_ORIGIN),
    ("access-control-allow-methods", "GET,OPTIONS"),
    ("access-control-allow-headers", "Content-Type,Authorization"),
    ("access-control-max-age", "600"),
];

/// One answered request: its status, its `Access-Control-*` headers by lowercased name, and its
/// body verbatim.
struct Answered {
    status: u16,
    cors_headers: Vec<(String, String)>,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn cors_header(&self, name: &str) -> Option<&str> {
        self.cors_headers
            .iter()
            .find(|(served, _)| served == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    let mut cors_headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .filter(|(name, _)| name.as_str().starts_with("access-control-"))
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).to_string(),
            )
        })
        .collect();
    cors_headers.sort();
    Answered {
        status: parts.status.as_u16(),
        cors_headers,
        body,
    }
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes a
/// setup request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &host,
        &format!("https://{host}{path}"),
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
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
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &host,
        &format!("https://{host}{path}"),
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One awsQuery call to `signing_name` that has to answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
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
        "POST",
        host,
        &format!("https://{host}/"),
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(form),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup {signing_name} {action}: expected HTTP 200, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// The `responseParameters` key one response header is declared and mapped under:
/// `method.response.header.<Name>`, with the header name in the spelling a browser reads it by.
fn method_response_header(lowercased: &str) -> String {
    let name: String = lowercased
        .split('-')
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), characters.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-");
    format!("method.response.header.{name}")
}

/// One request this suite sends to the deployed stage.
struct Call<'c> {
    /// How the request reads as a browser's from another site.
    how: &'c str,
    /// The browser-set headers it carries beside the preflight ones.
    browser_headers: &'c [(&'c str, &'c str)],
}

#[test]
fn a_cors_enabled_resource_answers_the_preflight_and_the_real_cross_origin_request() {
    std::thread::Builder::new()
        .name("apigateway-mock-options-cors-preflight".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(BOOT_STACK_SIZE)
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a multi-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                let function_arn = deployed_function(&router).await;
                let api_id = cors_enabled_api(&router, &function_arn).await;

                let own_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
                let endpoint = "127.0.0.1:39468";
                let addressings: [(&str, &str, String); 2] = [
                    (
                        "the api's own execute-api host",
                        &own_host,
                        format!("https://{own_host}/{STAGE}/{RESOURCE_PATH}"),
                    ),
                    (
                        "the emulator's own endpoint",
                        endpoint,
                        format!(
                            "https://{endpoint}/_aws/execute-api/{api_id}/{STAGE}/\
                             {RESOURCE_PATH}"
                        ),
                    ),
                ];
                let calls = [
                    Call {
                        how: "an Origin alone",
                        browser_headers: &[("origin", PAGE_ORIGIN)],
                    },
                    Call {
                        how: "an Origin the browser classified cross-site",
                        browser_headers: &[
                            ("origin", PAGE_ORIGIN),
                            ("sec-fetch-site", "cross-site"),
                        ],
                    },
                    Call {
                        how: "no browser header at all",
                        browser_headers: &[],
                    },
                ];

                for (addressing, host, uri) in addressings {
                    for call in &calls {
                        // The PREFLIGHT: answered by API Gateway itself, with every declared
                        // header carrying the integration response's literal unquoted and
                        // nothing else.
                        let case = format!("{addressing}, {}, the preflight", call.how);
                        let mut headers: Vec<(&str, &str)> = call.browser_headers.to_vec();
                        headers.push(("access-control-request-method", "GET"));
                        headers.push(("access-control-request-headers", "content-type"));
                        let answered =
                            dispatch(&router, "OPTIONS", host, &uri, &headers, Bytes::new()).await;
                        assert_eq!(
                            answered.status,
                            200,
                            "case {case:?}: expected the MOCK OPTIONS method's 200, got {} with \
                             {:?} and {:?}",
                            answered.status,
                            answered.text(),
                            answered.cors_headers
                        );
                        assert_eq!(
                            answered.text(),
                            "",
                            "case {case:?}: expected the empty body the integration response's \
                             own template renders, got {:?}",
                            answered.text()
                        );
                        for (name, expected) in DECLARED_PREFLIGHT_HEADERS {
                            let actual = answered.cors_header(name);
                            assert_eq!(
                                actual,
                                Some(*expected),
                                "case {case:?}: expected the declared {name} {expected:?} with \
                                 no quotes, got {actual:?}"
                            );
                        }
                        let served: Vec<&str> = answered
                            .cors_headers
                            .iter()
                            .map(|(name, _)| name.as_str())
                            .collect();
                        let mut declared: Vec<&str> =
                            DECLARED_PREFLIGHT_HEADERS.iter().map(|(name, _)| *name).collect();
                        declared.sort_unstable();
                        assert_eq!(
                            served, declared,
                            "case {case:?}: expected exactly the mapped Access-Control-* \
                             headers {declared:?}, got {served:?}"
                        );

                        // The REAL REQUEST: answered by its own AWS_PROXY integration, which
                        // is performed and handed this request's own `Origin`.
                        let case = format!("{addressing}, {}, the real request", call.how);
                        let answered = dispatch(
                            &router,
                            "GET",
                            host,
                            &uri,
                            call.browser_headers,
                            Bytes::new(),
                        )
                        .await;
                        assert_eq!(
                            answered.status,
                            200,
                            "case {case:?}: expected the function's own 200, got {} with {:?}",
                            answered.status,
                            answered.text()
                        );
                        let reported = answered.json();
                        let expected = call
                            .browser_headers
                            .iter()
                            .find(|(name, _)| *name == "origin")
                            .map(|(_, value)| Value::String((*value).to_string()))
                            .unwrap_or(Value::Null);
                        assert_eq!(
                            reported.get("origin"),
                            Some(&expected),
                            "case {case:?}: expected the handler to be handed the request's own \
                             Origin {expected:?}, got {reported}"
                        );
                    }
                }
            });
        })
        .expect("spawn the suite thread")
        .join()
        .expect("the suite thread must not panic");
}

/// Deploy the stack that declares the function the real request's integration invokes, grant
/// API Gateway the invocation, and answer the ARN Lambda reports once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter) -> String {
    let created = query(
        router,
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
    let role_arn = element(&created, "Arn");

    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Items": {
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
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        CLOUDFORMATION_VERSION,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
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

    let granted = lambda(
        router,
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
        "setup AddPermission: expected HTTP 201 from Lambda, got {} with {:?}",
        granted.status,
        granted.text()
    );
    member("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// The api every case drives: one resource carrying the `MOCK` `OPTIONS` method AWS's CORS flow
/// adds — whose integration response maps the four `Access-Control-*` headers — and the
/// `AWS_PROXY` `GET` method the real request reaches, deployed to one stage. Answers the api id.
async fn cors_enabled_api(router: &MultiServiceRouter, function_arn: &str) -> String {
    let api = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "mock-options-cors-preflight" })),
        201,
    )
    .await;
    let api_id = member("CreateRestApi", &api, "id");
    let root = member("CreateRestApi", &api, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api_id}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource_id = member("CreateResource", &resource, "id");

    let options = format!("/restapis/{api_id}/resources/{resource_id}/methods/OPTIONS");
    apigw(
        router,
        "PutMethod(OPTIONS)",
        "PUT",
        &options,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration(OPTIONS)",
        "PUT",
        &format!("{options}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        })),
        201,
    )
    .await;
    let declared: serde_json::Map<String, Value> = DECLARED_PREFLIGHT_HEADERS
        .iter()
        .map(|(name, _)| (method_response_header(name), Value::Bool(false)))
        .collect();
    let mapped: serde_json::Map<String, Value> = DECLARED_PREFLIGHT_HEADERS
        .iter()
        .map(|(name, value)| {
            (
                method_response_header(name),
                Value::String(format!("'{value}'")),
            )
        })
        .collect();
    apigw(
        router,
        "PutMethodResponse(OPTIONS)",
        "PUT",
        &format!("{options}/responses/200"),
        Some(json!({ "responseParameters": declared })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegrationResponse(OPTIONS)",
        "PUT",
        &format!("{options}/integration/responses/200"),
        Some(json!({
            "responseTemplates": { "application/json": "" },
            "responseParameters": mapped,
        })),
        201,
    )
    .await;

    let get = format!("/restapis/{api_id}/resources/{resource_id}/methods/GET");
    apigw(
        router,
        "PutMethod(GET)",
        "PUT",
        &get,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration(GET)",
        "PUT",
        &format!("{get}/integration"),
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
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api_id}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api_id
}
