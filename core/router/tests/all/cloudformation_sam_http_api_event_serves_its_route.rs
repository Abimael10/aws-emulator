// SPDX-License-Identifier: CC0-1.0

//! What a client of a stack deployed from a SAM template is SERVED on the route an
//! `AWS::Serverless::Function`'s `HttpApi` event declares.
//!
//! The rule under test: **the `AWS::Serverless-2016-10-31` transform turns every `HttpApi`
//! event of a function into a route of the HTTP api the event names — or of the one the macro
//! adds for an event that names none — whose key is the event's `Method` and `Path` and whose
//! `AWS_PROXY` integration invokes that function at the event's default payload format
//! version `2.0`, with the `Globals` section standing in as the default for each property the
//! declaration does not state itself; the api's `$default` stage then serves those routes, so
//! a request one of them matches is answered by that function's own run on the `2.0` proxy
//! event — whose `version` is `2.0`, whose `routeKey` is the declared key and whose `rawPath`
//! is the path the request arrived on — carrying `access-control-allow-origin` exactly when
//! the api's `CorsConfiguration` admits the request's `Origin`, while a method and path no
//! route of the api matches is answered `404` with the gateway's own `{"message":"Not
//! Found"}`.** Four crates have to agree for that to hold (CloudFormation expands the macro
//! and provisions what it emits, API Gateway v2 resolves the stage and the route and performs
//! the proxy integration, AWS Lambda runs the deployment package, IAM answers for the role the
//! macro synthesizes), so the suite lives here rather than in any one of them, and it drives
//! the emulator's own assembled service set.
//!
//! The `sam deploy` of this template is the ordinary way such an api is shipped, and the
//! control-plane half of the expansion is pinned next door by
//! `crate::cloudformation_sam_http_api_definitions`; what this suite adds is the ANSWER a
//! client gets on the endpoint the stack publishes.
//!
//! The rows differ along what the rule generalises over: whether the api is the one the
//! template declares or the one the macro adds, the event's method (`GET`, `POST`, the
//! catch-all `ANY`), whether its path is literal, carries a variable or ends in a greedy
//! segment, the path the request actually arrives on, whether the request carries an `Origin`
//! the configuration admits, a foreign one, or none at all, and whether any route of the api
//! matches the request at all. Every expected value is computed here from what the template
//! declares and what the request carried — never read back out of the answer under test.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The one origin the template's `Globals.HttpApi.CorsConfiguration` admits, and one it does
/// not.
const ALLOWED_ORIGIN: &str = "https://sam.example.test";
const FOREIGN_ORIGIN: &str = "https://elsewhere.test";

/// The response header a CORS answer carries, as Fetch names it.
const ALLOW_ORIGIN_HEADER: &str = "access-control-allow-origin";

/// The runtime and handler the template's `Globals.Function` block states for every function
/// in it, and the memory size it states as the default.
const GLOBAL_RUNTIME: &str = "python3.13";
const GLOBAL_HANDLER: &str = "index.handler";
/// `Globals.Function.MemorySize`, and the size one declaration states for itself instead.
/// Both are inside `MemorySize`'s published range — `@range` min 128, max 32768
/// (`smithy_query(query="shape", shape="MemorySize")` on the Lambda model).
const GLOBAL_MEMORY_SIZE: i64 = 192;
const OWN_MEMORY_SIZE: i64 = 256;

/// How long to wait between polls of a clock-owed transition, and how long to keep looking.
const POLL: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(120);

/// The handler both functions are deployed with, inline in the template as `sam deploy`
/// carries it: it answers with the proxy envelope's OWN fields, which is what tells the
/// payload format version, the route that was selected and the path the request arrived on.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {
                "version": event.get("version"),
                "routeKey": event.get("routeKey"),
                "rawPath": event.get("rawPath"),
            }
        ),
    }
"#;

/// One answered request: its status, its headers and its body verbatim.
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
}

/// The emulator as it is ASSEMBLED, on one engine: the stack's apis and functions are held by
/// the same router the invokes below reach.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published
    // endpoint carries none, as a client of a public HTTP api sends none.
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

/// Call one CloudFormation operation over its awsQuery wire.
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

/// One AWS Lambda read over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, path: &str) -> Answered {
    dispatch(
        router,
        "GET",
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

/// `GetFunctionConfiguration` on one function, as a client of the declared name reads it.
async fn function_configuration(router: &MultiServiceRouter, name: &str) -> Answered {
    lambda(router, &format!("/2015-03-31/functions/{name}/configuration")).await
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`. The span
/// is that service's own, so nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let reported = function_configuration(router, name).await;
        if reported.json().get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda never reported {name} Active; last answer {:?}",
            reported.text()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One `HttpApi` event of a function: the method and path it routes, and the api it names, or
/// none at all — which is the event the macro adds its own api for.
fn http_event(path: &str, method: &str, api: Option<&str>) -> Value {
    let mut properties = json!({ "Path": path, "Method": method });
    if let (Some(api), Some(members)) = (api, properties.as_object_mut()) {
        members.insert("ApiId".to_string(), json!({ "Ref": api }));
    }
    json!({ "Type": "HttpApi", "Properties": properties })
}

/// The template the suite deploys: an `AWS::Serverless::HttpApi` the template declares, two
/// `AWS::Serverless::Function`s whose `HttpApi` events route through it and through the api
/// the macro adds, the `Globals` block that stands in for what neither function states, and
/// the CORS configuration `Globals.HttpApi` states for every HTTP api in the template.
fn template(declared_function: &str, implicit_function: &str) -> String {
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Parameters": {
            "DeclaredFunctionName": { "Type": "String", "Default": declared_function },
            "ImplicitFunctionName": { "Type": "String", "Default": implicit_function },
            "AllowedOrigin": { "Type": "String", "Default": ALLOWED_ORIGIN }
        },
        "Globals": {
            "Function": {
                "Runtime": GLOBAL_RUNTIME,
                "Handler": GLOBAL_HANDLER,
                "MemorySize": GLOBAL_MEMORY_SIZE
            },
            "HttpApi": {
                "CorsConfiguration": {
                    "AllowOrigins": [{ "Ref": "AllowedOrigin" }],
                    "AllowMethods": ["GET", "POST"]
                }
            }
        },
        "Resources": {
            "Api": { "Type": "AWS::Serverless::HttpApi" },
            "Declared": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "FunctionName": { "Ref": "DeclaredFunctionName" },
                    "InlineCode": HANDLER_SOURCE,
                    "Events": {
                        "Hello": http_event("/hello", "get", Some("Api")),
                        "Order": http_event("/orders/{id}", "post", Some("Api")),
                        "Rest": http_event("/any/{proxy+}", "any", Some("Api"))
                    }
                }
            },
            "Implicit": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "FunctionName": { "Ref": "ImplicitFunctionName" },
                    // The declaration's own memory size, which the global default yields to.
                    "MemorySize": OWN_MEMORY_SIZE,
                    "InlineCode": HANDLER_SOURCE,
                    "Events": { "Ping": http_event("/ping", "get", None) }
                }
            }
        },
        "Outputs": {
            "Endpoint": { "Value": { "Fn::GetAtt": ["Api", "ApiEndpoint"] } },
            "ImplicitEndpoint": {
                "Value": { "Fn::GetAtt": ["ServerlessHttpApi", "ApiEndpoint"] }
            }
        }
    })
    .to_string()
}

/// Which of the stack's two apis a row addresses: the one the template declares, or the one
/// the macro adds for the event that names no api.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressed {
    Declared,
    Implicit,
}

/// One row: the request a client makes on a published endpoint, and the answer the rule owes
/// it.
struct Row {
    case: &'static str,
    /// The api the request is addressed at.
    api: Addressed,
    /// The method and path the client uses.
    method: &'static str,
    path: &'static str,
    /// The `Origin` the request carries, or `None` for one carrying none.
    origin: Option<&'static str>,
    /// The `routeKey` of the event the request must be served by — `None` for a request no
    /// route of the api matches, which is answered by the gateway itself.
    route_key: Option<&'static str>,
    /// `access-control-allow-origin` the answer must carry, or `None` for an answer that must
    /// carry none.
    allow_origin: Option<&'static str>,
}

/// `404` with exactly this body is the gateway's own answer for a request it has nothing for;
/// the text is fixed and carries no variable part.
const NOT_FOUND_BODY: &str = r#"{"message":"Not Found"}"#;

const ROWS: &[Row] = &[
    Row {
        case: "the literal route of a declared api, from an admitted origin",
        api: Addressed::Declared,
        method: "GET",
        path: "/hello",
        origin: Some(ALLOWED_ORIGIN),
        route_key: Some("GET /hello"),
        allow_origin: Some(ALLOWED_ORIGIN),
    },
    Row {
        case: "the same route and the same run, from an origin the configuration does not \
               admit",
        api: Addressed::Declared,
        method: "GET",
        path: "/hello",
        origin: Some(FOREIGN_ORIGIN),
        route_key: Some("GET /hello"),
        allow_origin: None,
    },
    Row {
        case: "the same route for a client that carries no Origin at all",
        api: Addressed::Declared,
        method: "GET",
        path: "/hello",
        origin: None,
        route_key: Some("GET /hello"),
        allow_origin: None,
    },
    Row {
        case: "a POST event whose path carries a variable the request fills",
        api: Addressed::Declared,
        method: "POST",
        path: "/orders/7",
        origin: Some(ALLOWED_ORIGIN),
        route_key: Some("POST /orders/{id}"),
        allow_origin: Some(ALLOWED_ORIGIN),
    },
    Row {
        case: "an ANY event whose path ends in a greedy segment the request fills with two",
        api: Addressed::Declared,
        method: "GET",
        path: "/any/deep/leaf",
        origin: Some(ALLOWED_ORIGIN),
        route_key: Some("ANY /any/{proxy+}"),
        allow_origin: Some(ALLOWED_ORIGIN),
    },
    Row {
        case: "the route of the api the macro adds for an event that names none",
        api: Addressed::Implicit,
        method: "GET",
        path: "/ping",
        origin: Some(ALLOWED_ORIGIN),
        route_key: Some("GET /ping"),
        allow_origin: Some(ALLOWED_ORIGIN),
    },
    Row {
        case: "a path the template declares no event for, on the declared api",
        api: Addressed::Declared,
        method: "GET",
        path: "/missing",
        origin: None,
        route_key: None,
        allow_origin: None,
    },
    Row {
        case: "a declared path asked for with a method no event of it declares",
        api: Addressed::Declared,
        method: "POST",
        path: "/hello",
        origin: None,
        route_key: None,
        allow_origin: None,
    },
    Row {
        case: "a path the template declares no event for, on the api the macro adds",
        api: Addressed::Implicit,
        method: "GET",
        path: "/nothing-here",
        origin: None,
        route_key: None,
        allow_origin: None,
    },
];

/// One row of the `Globals` table: the function, the memory size it must report, and why.
struct Merged {
    case: &'static str,
    /// Which of the two functions, by the parameter its name came from.
    declared: bool,
    memory_size: i64,
}

const MERGED: &[Merged] = &[
    Merged {
        case: "a function that states no memory size takes the Globals default",
        declared: true,
        memory_size: GLOBAL_MEMORY_SIZE,
    },
    Merged {
        case: "a function that states its own memory size overrides the Globals default",
        declared: false,
        memory_size: OWN_MEMORY_SIZE,
    },
];

/// An announced `ApiEndpoint` split into the authority a request is addressed to and the path
/// prefix it carries: this emulator announces its own address with the api's id in the path,
/// while the form `Api$ApiEndpoint` documents carries the id in the host and no path at all.
/// A client dials whichever it was handed, which is what this suite does.
fn addressed(endpoint: &str) -> (String, String) {
    let authority_and_path = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint)
        .trim_end_matches('/');
    match authority_and_path.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}")),
        None => (authority_and_path.to_string(), String::new()),
    }
}

// The emulator boots a multi-thread runtime (`#[tokio::main]`), and the seam that serves an
// invoke performs the route's integration as a task of the runtime it is served on, so the
// suite drives the router on that same flavour.
#[tokio::test(flavor = "multi_thread")]
async fn a_sam_http_api_event_serves_its_route_on_the_two_zero_proxy_event() {
    let router = emulator();

    // 1. `sam deploy` is CreateStack of the template with the macro acknowledged.
    let stack = "sam-httpapi-event-serves";
    let declared_function = format!("{stack}-declared");
    let implicit_function = format!("{stack}-implicit");
    let body = template(&declared_function, &implicit_function);
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            // The role the transform synthesizes for each function is acknowledged with
            // CAPABILITY_IAM, and the macro itself with CAPABILITY_AUTO_EXPAND.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    let failures = if reached == CREATE_COMPLETE {
        Vec::new()
    } else {
        let events =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events.text(), "ResourceStatusReason")
    };
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described} with resource \
         failures {failures:?}"
    );

    // 2. The `Globals.Function` block reached each generated function: the one that states
    // nothing takes the global default, the one that states its own size keeps it, and both
    // take the global runtime and handler.
    for merged in MERGED {
        let case = merged.case;
        let name = if merged.declared {
            &declared_function
        } else {
            &implicit_function
        };
        let read = function_configuration(&router, name).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration on {name}, got {} \
             with {:?}",
            read.status,
            read.text()
        );
        let configuration = read.json();
        let reported = configuration.get("MemorySize").and_then(Value::as_i64);
        assert_eq!(
            reported,
            Some(merged.memory_size),
            "case {case:?}: expected MemorySize {}, got {reported:?} in {configuration}",
            merged.memory_size
        );
        let runtime = configuration.get("Runtime").and_then(Value::as_str);
        assert_eq!(
            runtime,
            Some(GLOBAL_RUNTIME),
            "case {case:?}: expected the Globals runtime {GLOBAL_RUNTIME:?}, got {runtime:?} \
             in {configuration}"
        );
        let handler = configuration.get("Handler").and_then(Value::as_str);
        assert_eq!(
            handler,
            Some(GLOBAL_HANDLER),
            "case {case:?}: expected the Globals handler {GLOBAL_HANDLER:?}, got {handler:?} \
             in {configuration}"
        );
    }
    wait_until_active(&router, &declared_function).await;
    wait_until_active(&router, &implicit_function).await;

    // 3. The endpoints the stack published, each resolved from the api resource's own
    // `ApiEndpoint` attribute, which is what a client is handed.
    let outputs = outputs_of(&described);
    let endpoint_of = |name: &str| -> (String, String) {
        let announced = outputs
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("setup: no {name} output in {described}"));
        assert!(
            !announced.is_empty(),
            "setup: the stack's {name} output resolved to nothing in {described}"
        );
        addressed(&announced)
    };
    let declared_endpoint = endpoint_of("Endpoint");
    let implicit_endpoint = endpoint_of("ImplicitEndpoint");
    assert_ne!(
        declared_endpoint, implicit_endpoint,
        "setup: the declared api and the one the macro adds are two apis, so they are \
         announced on two endpoints, got {declared_endpoint:?} twice"
    );

    // 4. Every row, unsigned, on the endpoint the stack announced — as a client of a
    // published HTTP api calls it.
    for row in ROWS {
        let case = row.case;
        let (authority, prefix) = match row.api {
            Addressed::Declared => &declared_endpoint,
            Addressed::Implicit => &implicit_endpoint,
        };
        let addressed_path = format!("{prefix}{}", row.path);
        let headers: Vec<(&str, &str)> = row
            .origin
            .map(|origin| vec![("origin", origin)])
            .unwrap_or_default();
        let served = dispatch(
            &router,
            row.method,
            authority,
            &addressed_path,
            &headers,
            Bytes::new(),
        )
        .await;
        match row.route_key {
            Some(route_key) => {
                assert_eq!(
                    served.status,
                    200,
                    "case {case:?}: expected HTTP 200 from {} {authority}{addressed_path}, got \
                     {} with {:?}",
                    row.method,
                    served.status,
                    served.text()
                );
                // The handler answers with the proxy envelope's own fields, so this is what
                // the event it ran on carried: the format version the `HttpApi` event
                // defaults to, the key of the route that selected it, and the path the
                // request arrived on.
                let expected = json!({
                    "version": "2.0",
                    "routeKey": route_key,
                    "rawPath": row.path,
                });
                let answered = served.json();
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected the handler's own answer {expected}, got \
                     {answered}"
                );
            }
            None => {
                assert_eq!(
                    served.status,
                    404,
                    "case {case:?}: expected HTTP 404 from {} {authority}{addressed_path}, got \
                     {} with {:?}",
                    row.method,
                    served.status,
                    served.text()
                );
                assert_eq!(
                    served.text(),
                    NOT_FOUND_BODY,
                    "case {case:?}: expected the gateway's own not-found body \
                     {NOT_FOUND_BODY}, got {:?}",
                    served.text()
                );
            }
        }
        let allow_origin = served.header(ALLOW_ORIGIN_HEADER);
        assert_eq!(
            allow_origin, row.allow_origin,
            "case {case:?}: expected {ALLOW_ORIGIN_HEADER} {:?}, got {allow_origin:?} in {:?}",
            row.allow_origin, served.headers
        );
    }
}
