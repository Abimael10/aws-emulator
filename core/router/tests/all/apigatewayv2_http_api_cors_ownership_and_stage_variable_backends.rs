// SPDX-License-Identifier: CC0-1.0

//! Who owns the `Access-Control-*` headers of an HTTP api's answer, and which function a
//! `${stageVariables.*}` integration reaches — both driven end to end against a REAL AWS
//! Lambda backend.
//!
//! Three crates, so this suite lives here: CloudFormation archives each handler's inline
//! source into a deployment package, AWS Lambda runs it, and API Gateway v2's `execute-api`
//! HTTP data plane resolves the stage, the route and the integration in front of it. Only
//! here can a row observe a header the BACKEND set, which is what the CORS ownership rule is
//! about.
//!
//! The rules the rows assert, each stated one level above its recorded case:
//!
//! > **(1) The configuration owns the CORS headers.** An HTTP api that carries a
//! > `CorsConfiguration` owns every `Access-Control-*` header of whatever it answers: the
//! > gateway removes the ones the integration returned and states its own. An `Origin` its
//! > `AllowOrigins` admits is echoed in `access-control-allow-origin` — beside
//! > `ExposeHeaders`, and `access-control-allow-credentials: true` where `AllowCredentials`
//! > is set — and an `Origin` it does not admit is answered with the integration's status and
//! > body unchanged but NO `Access-Control-*` header at all, so the backend's own value never
//! > reaches the caller. A response header that is not a CORS header passes through untouched
//! > either way. An api carrying NO configuration owns none of them, so the integration's own
//! > reach the caller as it set them.
//! >
//! > **(2) The gateway answers the preflight.** For an api that carries a configuration the
//! > gateway answers the CORS PREFLIGHT itself — the `OPTIONS` request carrying `Origin` and
//! > `Access-Control-Request-Method` — wherever no route of the served deployment matches it:
//! > `204` with no body, no integration run, and the configured `AllowMethods`,
//! > `AllowHeaders` and `MaxAge` beside the echoed origin. A preflight whose origin, or whose
//! > asked-about method, the configuration does not admit comes back with no
//! > `access-control-allow-origin` at all. An api carrying no configuration answers an
//! > `OPTIONS` request from its routes alone, so one no route matches is its `404`.
//! >
//! > **(3) A stage variable selects the backend.** An integration's `IntegrationUri` may name
//! > its backend through `${stageVariables.<name>}`: the gateway resolves the reference
//! > against the stage SERVING the request, so one integration and one deployment reach a
//! > different Lambda function per stage, and the `2.0` event each function is handed carries
//! > the serving stage in `requestContext.stage` and that stage's own variables in
//! > `stageVariables`. A reference the serving stage carries no variable for names no backend
//! > at all, which is answered as an integration that produced no response (`500`
//! > `{"message":"Internal Server Error"}`) rather than left unanswered.
//!
//! Each `Cors` member's meaning is its own (`smithy_query(query="shape", shape="Cors")`):
//! `AllowOrigins` "a collection of allowed origins", `AllowMethods` "a collection of allowed
//! HTTP methods", `AllowHeaders` "a collection of allowed headers", `ExposeHeaders` "a
//! collection of exposed headers", `AllowCredentials` "whether credentials are included in
//! the CORS request", `MaxAge` "the number of seconds that the browser should cache preflight
//! request results"; `Api$CorsConfiguration` ("Supported only for HTTP APIs") is why the
//! GATEWAY and not a backend answers for them. `StageVariables` is `CreateStage`'s own
//! member, and `Integration$IntegrationUri` is where the reference is written.
//!
//! The rows span what the rules generalise over: how many origins the collection holds and
//! which of them the request carries, the wildcard entry, an origin the collection does not
//! hold, a request carrying no `Origin` at all, which `Cors` members the configuration states
//! (including none at all), whether the request is a preflight, whether the method it asks
//! about is admitted — and, for the stage-variable rows, which stage serves the request and
//! whether that stage carries the variable the URI names.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "eu-west-1";

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `StackStatus` a stack whose resources could not all be made reports, which is what
/// the setup names when it did not reach [`CREATE_COMPLETE`].
const ROLLBACK_SUFFIX: &str = "FAILED";

/// Two origins one configuration allow-lists, and one no configuration here holds.
const ALLOWED_ORIGIN: &str = "https://app.allowed.test";
const SECOND_ORIGIN: &str = "https://admin.allowed.test";
const FOREIGN_ORIGIN: &str = "https://other.foreign.test";

/// The origin the BACKEND itself names in the `Access-Control-Allow-Origin` it returns: the
/// value an api whose configuration governs these headers must never pass through.
const BACKEND_ORIGIN: &str = "https://backend.chose.test";

/// The entry a CORS list carries to stand for every value.
const WILDCARD: &str = "*";

/// The response header the backend sets that is NOT a CORS header, and its value: it passes
/// through whatever the configuration decides about the CORS ones.
const MARKER_HEADER: &str = "x-marker";
const MARKER_VALUE: &str = "backend-served";

/// The prefix of every response header rules (1) and (2) are about, so a row can assert the
/// answer carries EXACTLY the headers its api owes and nothing else.
const CORS_HEADER_PREFIX: &str = "access-control-";

/// The status the GATEWAY answers a preflight no route matches with, and the status a request
/// no route matches earns from an api that governs no CORS answer.
const PREFLIGHT: u16 = 204;
const NOT_FOUND: u16 = 404;

/// The status and sentence an api answers when the route's integration produced no response —
/// which is what an `IntegrationUri` naming a variable the serving stage does not carry comes
/// to.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// How long one invocation of these handlers is given, as each function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How often a clock-owed transition is looked at again, and how long the look goes on for.
/// The deadline is generous: the condition decides the outcome, never the host's speed.
const LOOK: Duration = Duration::from_millis(50);
const LOOK_DEADLINE: Duration = Duration::from_secs(60);

/// The handler the CORS rows run: it answers `200` with a body of its own and a header set
/// that deliberately includes every `Access-Control-*` header an api's configuration governs,
/// plus one header that is not a CORS header at all.
const CORS_BACKEND_SOURCE: &str = r#"
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {
            "Content-Type": "application/json",
            "Access-Control-Allow-Origin": "BACKEND_ORIGIN",
            "Access-Control-Allow-Methods": "DELETE",
            "Access-Control-Allow-Headers": "x-backend-header",
            "Access-Control-Expose-Headers": "x-backend-only",
            "Access-Control-Allow-Credentials": "true",
            "Access-Control-Max-Age": "99",
            "MARKER_HEADER": "MARKER_VALUE",
        },
        "body": json.dumps(
            {
                "backend": "cors-backend",
                "method": event["requestContext"]["http"]["method"],
            }
        ),
    }
"#;

/// The handler the stage-variable rows run: it reports WHICH function answered and what the
/// `2.0` event it was handed says about the stage that served the request.
const STAGE_BACKEND_SOURCE: &str = r#"
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {
                "backend": "MARKER",
                "version": event.get("version"),
                "routeKey": event.get("routeKey"),
                "stage": event["requestContext"]["stage"],
                "stageVariables": event.get("stageVariables") or {},
            }
        ),
    }
"#;

/// One answered request: its status, its headers by lowercased name, and its body verbatim.
struct Answered {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// Exactly the `Access-Control-*` headers the answer carries.
    fn cors(&self) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .filter(|(name, _)| name.starts_with(CORS_HEADER_PREFIX))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the stack's functions and the apis the
/// control plane makes are held by the same router the invokes below reach.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and an invoke of a published stage
    // carries none, as a client of one sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
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
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect(),
        body,
    }
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::new(),
    )
    .await
}

/// One API Gateway v2 control-plane call over its `restJson1` wire, which must answer
/// `expected`.
async fn apigatewayv2(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body
        .as_ref()
        .map_or_else(Bytes::new, |body| Bytes::from(body.to_string()));
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        payload,
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

/// A member of a control-plane answer, as the string it must be.
fn as_str(case: &str, document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: expected a {member} in {document}"))
        .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, which is
/// the state an invocation needs. The span is that service's own, so nothing here sleeps a
/// fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + LOOK_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
        )
        .await;
        last = reported.json();
        if last.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda never reported {name} Active, last reported {last}");
}

/// The template that provisions the suite's functions: the execution role, one function per
/// `(logical id, source)` pair with its code INLINE — which is how a handler written here
/// comes to exist as a deployment package at all — and, for each, the resource policy that
/// admits API Gateway's invocation.
fn function_stack(functions: &[(&str, String)]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole"
                    }]
                }
            }
        }),
    );
    let mut outputs = serde_json::Map::new();
    for (logical_id, source) in functions {
        resources.insert(
            (*logical_id).to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": 128,
                    "Code": { "ZipFile": source }
                }
            }),
        );
        resources.insert(
            format!("{logical_id}Permission"),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": [*logical_id, "Arn"] },
                    "Principal": "apigateway.amazonaws.com"
                }
            }),
        );
        outputs.insert(
            format!("{logical_id}Name"),
            json!({ "Value": { "Ref": *logical_id } }),
        );
        outputs.insert(
            format!("{logical_id}Arn"),
            json!({ "Value": { "Fn::GetAtt": [*logical_id, "Arn"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs)
    })
    .to_string()
}

/// Deploy that stack, wait for it, and answer its outputs: each function's name and ARN.
async fn deploy_functions(
    router: &MultiServiceRouter,
    stack: &str,
    functions: &[(&str, String)],
) -> BTreeMap<String, String> {
    let body = function_stack(functions);
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    assert!(
        created.text().contains("<StackId>"),
        "setup: expected CreateStack to report a StackId, got {:?}",
        created.text()
    );
    let (reached, described) = wait_for_stack(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {reached:?} with \
         {described} and the resource failures {:?}",
        resource_failures(router, stack).await
    );
    let outputs = outputs_of(&described);
    for (logical_id, _) in functions {
        let name = outputs
            .get(&format!("{logical_id}Name"))
            .cloned()
            .unwrap_or_else(|| panic!("setup: no {logical_id}Name output in {described}"));
        // The function the stack published really exists in AWS Lambda: that service's own
        // read answers for it, rather than a function nothing created.
        let read = lambda(router, "GET", &format!("/2015-03-31/functions/{name}")).await;
        assert_eq!(
            read.status, 200,
            "setup: expected HTTP 200 from GetFunction on {name:?}, got {} with {:?}",
            read.status,
            read.text()
        );
        wait_until_active(router, &name).await;
    }
    outputs
}

/// Poll `DescribeStacks` until the stack first reports `wanted`, and answer the last status
/// it reported with the answer that status was read from. The wait is on that CONDITION — a
/// stack is complete once the services holding its resources report them ready, which no
/// fixed sleep can name — and it stops early on a status that will not change again.
async fn wait_for_stack(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let giving_up_at = Instant::now() + LOOK_DEADLINE;
    loop {
        let described = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let body = described.text();
        let reported = if described.status == 200 {
            opt_element(&body, "StackStatus").unwrap_or_default()
        } else {
            String::new()
        };
        if reported == wanted || reported.ends_with(ROLLBACK_SUFFIX) {
            return (reported, body);
        }
        if Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// Every reason a resource of `stack` reported for not being made, so a setup that did not
/// reach [`CREATE_COMPLETE`] says WHY rather than only that it did not.
async fn resource_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let events = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events.text(), "ResourceStatusReason")
}

/// One request made on a deployed stage, and what the api owes its answer.
struct Leg {
    case: &'static str,
    method: &'static str,
    /// The `Origin` the request carries, or `None` for a request carrying none.
    origin: Option<&'static str>,
    /// The method a preflight asks about, which is what makes the request a preflight.
    asked_about: Option<&'static str>,
    /// The header list a preflight asks about, where it names one.
    asked_headers: Option<&'static str>,
    status: u16,
    /// EXACTLY the `Access-Control-*` headers the answer must carry.
    cors: &'static [(&'static str, &'static str)],
    /// Whether the INTEGRATION's own answer — the handler's `200`, its body and its
    /// non-CORS header — is what was served.
    served_by_the_function: bool,
}

/// One api: the configuration it carries, and the requests made on the stage that serves it.
struct Scenario {
    case: &'static str,
    /// The `CorsConfiguration` the api is created with, or `None` for an api carrying none.
    cors: Option<Value>,
    legs: &'static [Leg],
}

fn scenarios() -> Vec<Scenario> {
    vec![
    Scenario {
        case: "two allow-listed origins, one allowed method, two allowed headers, one exposed \
               header and a max age",
        cors: Some(json!({
                "allowOrigins": [ALLOWED_ORIGIN, SECOND_ORIGIN],
                "allowMethods": ["GET"],
                "allowHeaders": ["content-type", "x-trace"],
                "exposeHeaders": [MARKER_HEADER],
                "maxAge": 600
        })),
        legs: &[
            Leg {
                case: "the first allow-listed origin's request",
                method: "GET",
                origin: Some(ALLOWED_ORIGIN),
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[
                    ("access-control-allow-origin", ALLOWED_ORIGIN),
                    ("access-control-expose-headers", MARKER_HEADER),
                ],
                served_by_the_function: true,
            },
            Leg {
                case: "the second allow-listed origin's request",
                method: "GET",
                origin: Some(SECOND_ORIGIN),
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[
                    ("access-control-allow-origin", SECOND_ORIGIN),
                    ("access-control-expose-headers", MARKER_HEADER),
                ],
                served_by_the_function: true,
            },
            Leg {
                case: "a request from an origin the collection does not hold: the backend's \
                       own CORS headers must not leak through",
                method: "GET",
                origin: Some(FOREIGN_ORIGIN),
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[],
                served_by_the_function: true,
            },
            Leg {
                case: "a request carrying no Origin at all",
                method: "GET",
                origin: None,
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[],
                served_by_the_function: true,
            },
            Leg {
                case: "the preflight of an allow-listed origin about an allowed method, which \
                       no route of the api matches",
                method: "OPTIONS",
                origin: Some(ALLOWED_ORIGIN),
                asked_about: Some("GET"),
                asked_headers: Some("content-type"),
                status: PREFLIGHT,
                cors: &[
                    ("access-control-allow-origin", ALLOWED_ORIGIN),
                    ("access-control-allow-methods", "GET"),
                    ("access-control-allow-headers", "content-type,x-trace"),
                    ("access-control-max-age", "600"),
                ],
                served_by_the_function: false,
            },
            Leg {
                case: "the preflight of an origin the collection does not hold",
                method: "OPTIONS",
                origin: Some(FOREIGN_ORIGIN),
                asked_about: Some("GET"),
                asked_headers: None,
                status: PREFLIGHT,
                cors: &[],
                served_by_the_function: false,
            },
            Leg {
                case: "the preflight of an allow-listed origin about a method the \
                       configuration does not allow",
                method: "OPTIONS",
                origin: Some(ALLOWED_ORIGIN),
                asked_about: Some("DELETE"),
                asked_headers: None,
                status: PREFLIGHT,
                cors: &[],
                served_by_the_function: false,
            },
        ],
    },
    Scenario {
        case: "the wildcard entry, which admits every origin and every method",
        cors: Some(json!({
                "allowOrigins": [WILDCARD],
                "allowMethods": [WILDCARD],
                "allowHeaders": [WILDCARD],
                "maxAge": 0
        })),
        legs: &[
            Leg {
                case: "a request from an origin no entry names, admitted by the wildcard",
                method: "GET",
                origin: Some(FOREIGN_ORIGIN),
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[("access-control-allow-origin", WILDCARD)],
                served_by_the_function: true,
            },
            Leg {
                case: "the preflight of that origin about a method no entry names",
                method: "OPTIONS",
                origin: Some(FOREIGN_ORIGIN),
                asked_about: Some("DELETE"),
                asked_headers: Some("content-type"),
                status: PREFLIGHT,
                cors: &[
                    ("access-control-allow-origin", WILDCARD),
                    ("access-control-allow-methods", WILDCARD),
                    ("access-control-allow-headers", WILDCARD),
                    ("access-control-max-age", "0"),
                ],
                served_by_the_function: false,
            },
        ],
    },
    Scenario {
        case: "a credentialed configuration: one origin, one method, AllowCredentials",
        cors: Some(json!({
                "allowOrigins": [ALLOWED_ORIGIN],
                "allowMethods": ["GET"],
                "allowCredentials": true
        })),
        legs: &[
            Leg {
                case: "the allow-listed origin's credentialed request",
                method: "GET",
                origin: Some(ALLOWED_ORIGIN),
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[
                    ("access-control-allow-origin", ALLOWED_ORIGIN),
                    ("access-control-allow-credentials", "true"),
                ],
                served_by_the_function: true,
            },
            Leg {
                case: "a request from an origin the collection does not hold: neither the \
                       backend's origin nor its credentials flag may be returned",
                method: "GET",
                origin: Some(FOREIGN_ORIGIN),
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[],
                served_by_the_function: true,
            },
            Leg {
                case: "the allow-listed origin's preflight",
                method: "OPTIONS",
                origin: Some(ALLOWED_ORIGIN),
                asked_about: Some("GET"),
                asked_headers: None,
                status: PREFLIGHT,
                cors: &[
                    ("access-control-allow-origin", ALLOWED_ORIGIN),
                    ("access-control-allow-methods", "GET"),
                    ("access-control-allow-credentials", "true"),
                ],
                served_by_the_function: false,
            },
        ],
    },
    Scenario {
        case: "an api carrying NO CorsConfiguration, which owns none of these headers",
        cors: None,
        legs: &[
            Leg {
                case: "a request whose answer carries the backend's own CORS headers, \
                       untouched",
                method: "GET",
                origin: Some(ALLOWED_ORIGIN),
                asked_about: None,
                asked_headers: None,
                status: 200,
                cors: &[
                    ("access-control-allow-credentials", "true"),
                    ("access-control-allow-headers", "x-backend-header"),
                    ("access-control-allow-methods", "DELETE"),
                    ("access-control-allow-origin", BACKEND_ORIGIN),
                    ("access-control-expose-headers", "x-backend-only"),
                    ("access-control-max-age", "99"),
                ],
                served_by_the_function: true,
            },
            Leg {
                case: "a preflight no route of the api matches, which the gateway answers for \
                       no configuration",
                method: "OPTIONS",
                origin: Some(ALLOWED_ORIGIN),
                asked_about: Some("GET"),
                asked_headers: None,
                status: NOT_FOUND,
                cors: &[],
                served_by_the_function: false,
            },
        ],
    },
    ]
}

/// The route every CORS scenario holds — a `GET` route, and no `OPTIONS` or `$default` route,
/// so a preflight is matched by nothing.
const CORS_ROUTE_KEY: &str = "GET /cors";
const CORS_ROUTE_PATH: &str = "/cors";

// The emulator boots a multi-thread runtime (`#[tokio::main]`), and the seam that serves an
// invoke performs the route's integration as a task of the runtime it is served on, so the
// suite drives the router on that same flavour.
#[tokio::test(flavor = "multi_thread")]
async fn an_apis_cors_configuration_owns_the_cors_headers_of_what_its_lambda_route_answered() {
    let router = emulator();
    let source = CORS_BACKEND_SOURCE
        .replace("BACKEND_ORIGIN", BACKEND_ORIGIN)
        .replace("MARKER_HEADER", MARKER_HEADER)
        .replace("MARKER_VALUE", MARKER_VALUE);
    let outputs = deploy_functions(
        &router,
        "apigatewayv2-cors-backend",
        &[("CorsBackend", source)],
    )
    .await;
    let function_arn = outputs
        .get("CorsBackendArn")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no CorsBackendArn output in {outputs:?}"));

    for scenario in scenarios() {
        let case = scenario.case;
        // One api per scenario, every one fronting the SAME function: what differs between
        // them is the configuration, never the backend.
        let mut request = json!({ "name": "cors-ownership-api", "protocolType": "HTTP" });
        if let Some(cors) = scenario.cors {
            request["corsConfiguration"] = cors;
        }
        let api = apigatewayv2(&router, case, "POST", "/v2/apis", Some(request), 201).await;
        let api_id = as_str(case, &api, "apiId");
        let integration = apigatewayv2(
            &router,
            case,
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": function_arn,
                "payloadFormatVersion": "2.0"
            })),
            201,
        )
        .await;
        let integration_id = as_str(case, &integration, "integrationId");
        apigatewayv2(
            &router,
            case,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": CORS_ROUTE_KEY,
                "target": format!("integrations/{integration_id}")
            })),
            201,
        )
        .await;
        apigatewayv2(
            &router,
            case,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": "$default", "autoDeploy": true })),
            201,
        )
        .await;

        let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
        for leg in scenario.legs {
            let mut headers: Vec<(&str, &str)> = Vec::new();
            if let Some(origin) = leg.origin {
                headers.push(("origin", origin));
            }
            if let Some(asked_about) = leg.asked_about {
                headers.push(("access-control-request-method", asked_about));
            }
            if let Some(asked_headers) = leg.asked_headers {
                headers.push(("access-control-request-headers", asked_headers));
            }
            let served = dispatch(
                &router,
                leg.method,
                &host,
                CORS_ROUTE_PATH,
                &headers,
                Bytes::new(),
            )
            .await;
            let at = format!("{case:?} / {:?}", leg.case);
            assert_eq!(
                served.status, leg.status,
                "case {at}: expected HTTP {} from {} {host}{CORS_ROUTE_PATH}, got {} with \
                 {:?}",
                leg.status,
                leg.method,
                served.status,
                served.text()
            );
            let answered = served.cors();
            let expected: BTreeMap<String, String> = leg
                .cors
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                answered, expected,
                "case {at}: expected exactly the CORS headers {expected:?}, got \
                 {answered:?}"
            );
            if leg.served_by_the_function {
                // The integration's own answer reached the caller: its body, and the header
                // it set that is no part of any CORS rule.
                assert_eq!(
                    served.json(),
                    json!({ "backend": "cors-backend", "method": leg.method }),
                    "case {at}: expected the handler's own answer, got {:?}",
                    served.text()
                );
                assert_eq!(
                    served.header(MARKER_HEADER),
                    Some(MARKER_VALUE),
                    "case {at}: a header that is not a CORS header passes through, got \
                     {:?}",
                    served.headers
                );
            } else if leg.status == PREFLIGHT {
                // The GATEWAY answered it, so no integration ran and there is no body.
                assert!(
                    served.body.is_empty(),
                    "case {at}: a preflight the gateway answers carries no body, got {:?}",
                    served.text()
                );
                assert_eq!(
                    served.header(MARKER_HEADER),
                    None,
                    "case {at}: no integration may run for a preflight the gateway \
                     answers, got {:?}",
                    served.headers
                );
            } else {
                assert_eq!(
                    served.json(),
                    json!({ "message": "Not Found" }),
                    "case {at}: expected the gateway's own not-found body, got {:?}",
                    served.text()
                );
            }
        }
    }
}

/// One stage of the stage-variable api: its name, the variables it carries, and what a
/// request on its invoke URL is answered with.
struct StageRow {
    case: &'static str,
    stage: &'static str,
    /// The logical id of the function this stage's `fn` variable names, or `None` for a stage
    /// that carries no `fn` variable at all.
    backend: Option<&'static str>,
    status: u16,
}

const STAGE_ROWS: &[StageRow] = &[
    StageRow {
        case: "the first stage, whose variable names the first function",
        stage: "blue",
        backend: Some("BlueBackend"),
        status: 200,
    },
    StageRow {
        case: "the second stage, whose variable names the second function",
        stage: "green",
        backend: Some("GreenBackend"),
        status: 200,
    },
    StageRow {
        case: "a stage carrying no variable of that name, which names no backend",
        stage: "unset",
        backend: None,
        status: INTEGRATION_FAULT,
    },
];

/// The variable the integration's URI names.
const BACKEND_VARIABLE: &str = "fn";

#[tokio::test(flavor = "multi_thread")]
async fn one_integration_reaches_the_function_the_serving_stages_variable_names() {
    let router = emulator();
    let outputs = deploy_functions(
        &router,
        "apigatewayv2-stage-variable-backends",
        &[
            (
                "BlueBackend",
                STAGE_BACKEND_SOURCE.replace("MARKER", "blue-backend"),
            ),
            (
                "GreenBackend",
                STAGE_BACKEND_SOURCE.replace("MARKER", "green-backend"),
            ),
        ],
    )
    .await;
    let name_of = |logical_id: &str| {
        outputs
            .get(&format!("{logical_id}Name"))
            .cloned()
            .unwrap_or_else(|| panic!("setup: no {logical_id}Name output in {outputs:?}"))
    };
    // The ARN the integration is written with names its function through a stage variable, so
    // the account and the Region come from an ARN the stack's own function really has.
    let published = outputs
        .get("BlueBackendArn")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no BlueBackendArn output in {outputs:?}"));
    let account = published
        .split(':')
        .nth(4)
        .unwrap_or_else(|| panic!("setup: no account in the published ARN {published:?}"))
        .to_string();
    let templated_uri =
        format!("arn:aws:lambda:{REGION}:{account}:function:${{stageVariables.{BACKEND_VARIABLE}}}");

    let case = "the stage-variable api";
    let api = apigatewayv2(
        &router,
        case,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "stage-variable-api", "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api_id = as_str(case, &api, "apiId");
    // ONE integration and ONE route: what differs per stage is the variable, not the records.
    let integration = apigatewayv2(
        &router,
        case,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": templated_uri,
            "payloadFormatVersion": "2.0"
        })),
        201,
    )
    .await;
    let integration_id = as_str(case, &integration, "integrationId");
    let route_key = "GET /which";
    apigatewayv2(
        &router,
        case,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": route_key,
            "target": format!("integrations/{integration_id}")
        })),
        201,
    )
    .await;
    // ONE deployment, released to every stage below.
    let deployment = apigatewayv2(
        &router,
        case,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
        201,
    )
    .await;
    let deployment_id = as_str(case, &deployment, "deploymentId");

    let mut variables: BTreeMap<&str, BTreeMap<String, String>> = BTreeMap::new();
    for row in STAGE_ROWS {
        let stage_variables: BTreeMap<String, String> = match row.backend {
            Some(logical_id) => {
                BTreeMap::from([(BACKEND_VARIABLE.to_string(), name_of(logical_id))])
            }
            // A stage of the same api that carries a variable, but not the one the URI names.
            None => BTreeMap::from([("tenant".to_string(), "unset-stage".to_string())]),
        };
        apigatewayv2(
            &router,
            row.case,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({
                "stageName": row.stage,
                "deploymentId": deployment_id,
                "stageVariables": stage_variables
            })),
            201,
        )
        .await;
        variables.insert(row.stage, stage_variables);
    }

    let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
    for row in STAGE_ROWS {
        let case = row.case;
        let path = format!("/{}/which", row.stage);
        let served = dispatch(&router, "GET", &host, &path, &[], Bytes::new()).await;
        assert_eq!(
            served.status, row.status,
            "case {case:?}: expected HTTP {} from GET {host}{path}, got {} with {:?}",
            row.status,
            served.status,
            served.text()
        );
        let answered = served.json();
        match row.backend {
            Some(logical_id) => {
                let expected = json!({
                    "backend": format!("{}-backend", row.stage),
                    "version": "2.0",
                    "routeKey": route_key,
                    "stage": row.stage,
                    "stageVariables": variables.get(row.stage),
                });
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected {logical_id} to answer {expected}, got {answered}"
                );
            }
            None => {
                assert_eq!(
                    answered,
                    json!({ "message": INTEGRATION_FAULT_MESSAGE }),
                    "case {case:?}: expected the sentence an integration that produced no \
                     response earns, got {:?}",
                    served.text()
                );
            }
        }
    }
}
