// SPDX-License-Identifier: CC0-1.0

//! What an HTTP api's `execute-api` endpoint makes of the REQUEST TARGET before it selects a
//! route, and how every answer it gives NAMES the request it answered.
//!
//! Two rules, both about the data plane the apigatewayv2 model binds no operation of (it binds
//! the control plane — `CreateApi`, `CreateRoute`, `CreateStage` — alone), so both are the
//! recorded contract of that endpoint:
//!
//! > **RULE 1 — the target.** An HTTP api resolves a request against its route keys by the
//! > SEGMENTS of the request target: split on the `/` characters that arrived, every segment
//! > counted (so a trailing `/` is one empty last segment), the dot segments RFC 3986 §5.2.4
//! > removes taken out, and each remaining segment percent-decoded exactly TWICE — no more, so
//! > a third escaped layer survives as text. A segment whose own escape decodes to `/`
//! > separates nothing. `rawPath`, `requestContext.http.path` and `pathParameters` are built
//! > from those decoded segments, and a target carrying an escape either pass cannot read is
//! > refused by the gateway with `400` `{"message":"Bad Request"}` — which is what makes the
//! > second pass observable even where it decodes to the same text.
//!
//! > **RULE 2 — the request id.** Every response this endpoint returns — the gateway's own
//! > envelopes as much as a relayed integration response — carries the gateway's own
//! > `apigw-requestid` header, and its value is the id that same invoke reports in the proxy
//! > payload's `requestContext.requestId`. One id per request, so two requests carry two.
//!
//! Four crates have to agree for either to hold — CloudFormation archives the inline handler
//! into a deployment package this emulator can run, IAM holds the execution role the template
//! names, Lambda holds the function and RUNS it, and API Gateway v2 mints the api, resolves the
//! target to a route and performs the proxy integration — so the suite lives here rather than
//! in any one crate.
//!
//! Nothing here reads an emulator internal: every served row asserts the document the HANDLER
//! returned, which the gateway can only produce by decoding the target and invoking the
//! function with the event it built from it.
//!
//! The rows span what the rules generalise over: how many escaped layers the target spells
//! (none, one, two, three), WHAT an escape decodes to (a letter, a separator, a percent), the
//! shape of the path (a literal, a single-segment variable, the greedy remainder, a trailing
//! slash, both dot segments), whether a pass can read the target at all and which pass fails,
//! and — for the id — whether the answer came from the gateway (a method no route key names, a
//! target no pass can read) or from the integration.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The response header an HTTP api's endpoint names the request it answered in.
const GATEWAY_REQUEST_ID_HEADER: &str = "apigw-requestid";

/// What the gateway answers for a target no decoding pass can read, and for a method and path
/// no route key of the api names.
const BAD_REQUEST: u16 = 400;
const BAD_REQUEST_MESSAGE: &str = "Bad Request";
const NOT_FOUND: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Not Found";

/// How long one invocation of the handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long to wait between polls of a clock-owed transition, and how many polls to make. The
/// deadline is generous: what is waited for is a CONDITION another service reports, never a
/// span this suite measures.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 400;

/// The handler the stack deploys, inline in the template exactly as a synthesised app carries
/// it: it echoes the parts of the event this suite is about — which route key served the
/// request, the path the gateway decoded, the parameters it captured, the query as it arrived,
/// and the id the gateway minted for the invoke.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    request_context = event.get("requestContext") or {}
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {
                "routeKey": event.get("routeKey"),
                "rawPath": event.get("rawPath"),
                "rawQueryString": event.get("rawQueryString"),
                "pathParameters": event.get("pathParameters") or {},
                "contextPath": (request_context.get("http") or {}).get("path"),
                "requestId": request_context.get("requestId"),
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

    /// The `message` a gateway envelope carries, or `""` for a body carrying none.
    fn message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The member `member` of the document the HANDLER returned.
    fn echoed(&self, member: &str) -> Value {
        self.json().get(member).cloned().unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the stack's function and api are held by
/// the same router the invoke below reaches.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the deployed stage
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

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`. The span
/// is that service's own, so nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    for _ in 0..POLLS {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
        )
        .await;
        if reported.json().get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// The route keys the api deploys. Between them they span the shapes a key has: a literal, a
/// single-segment variable under it, the greedy remainder, and two literals that the dot
/// segments of a target resolve onto.
const ROUTE_KEYS: [&str; 5] = [
    "GET /items",
    "GET /items/{id}",
    "GET /pets/{proxy+}",
    "GET /a/b",
    "GET /b",
];

/// The template the suite deploys: the function with its code INLINE, the api, the `AWS_PROXY`
/// integration on that function, one route per key above, the auto-deployed `$default` stage
/// (the stage that serves the api's root, reached with no stage segment at all), and the
/// permission that admits the gateway's invocation.
fn template() -> String {
    let mut resources = json!({
        "Role": {
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
        },
        "Fn": {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                "Timeout": FUNCTION_TIMEOUT,
                "MemorySize": 128,
                "Code": { "ZipFile": HANDLER_SOURCE }
            }
        },
        "Api": {
            "Type": "AWS::ApiGatewayV2::Api",
            "Properties": {
                "Name": "target-decoding-api",
                "ProtocolType": "HTTP"
            }
        },
        "Integration": {
            "Type": "AWS::ApiGatewayV2::Integration",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "IntegrationType": "AWS_PROXY",
                "IntegrationUri": { "Fn::GetAtt": ["Fn", "Arn"] },
                "PayloadFormatVersion": "2.0"
            }
        },
        "Stage": {
            "Type": "AWS::ApiGatewayV2::Stage",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "StageName": "$default",
                "AutoDeploy": true
            }
        },
        "Permission": {
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "Action": "lambda:InvokeFunction",
                "FunctionName": { "Fn::GetAtt": ["Fn", "Arn"] },
                "Principal": "apigateway.amazonaws.com"
            }
        }
    });
    for (index, route_key) in ROUTE_KEYS.iter().enumerate() {
        resources[format!("Route{index}")] = json!({
            "Type": "AWS::ApiGatewayV2::Route",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "RouteKey": route_key,
                "Target": { "Fn::Join": ["/", ["integrations", { "Ref": "Integration" }]] }
            }
        });
    }
    json!({
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "FunctionName": { "Value": { "Ref": "Fn" } }
        }
    })
    .to_string()
}

/// Deploy the stack and answer `(the api's own endpoint host, the api id)`.
async fn deployed(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let body = template();
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
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let outputs = outputs_of(&described);
    let api_id = outputs
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no ApiId output in {described}"));
    let function_name = outputs
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no FunctionName output in {described}"));
    // The function the stack published really exists in AWS Lambda, and is runnable: a read of
    // it through that service's own API answers rather than reporting one nothing created.
    let read = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function_name}"),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "setup: expected HTTP 200 from GetFunction on {function_name:?}, got {} with {:?}",
        read.status,
        read.text()
    );
    wait_until_active(router, &function_name).await;
    (
        format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        api_id,
    )
}

/// One row of the target table: the target a client sends, and what the api must make of it.
struct Row {
    case: &'static str,
    /// The path as it is sent, escapes and all.
    target: &'static str,
    /// The route key that must serve it, or `None` for a target the GATEWAY answers itself.
    route_key: Option<&'static str>,
    /// The `rawPath` the event must report for a served target.
    raw_path: &'static str,
    /// The `pathParameters` it must report — written as JSON, empty for a key naming none.
    path_parameters: fn() -> Value,
    /// The status the client is answered.
    status: u16,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a literal segment, nothing to decode",
            target: "/items/plain",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/plain",
            path_parameters: || json!({ "id": "plain" }),
            status: 200,
        },
        Row {
            // The recorded case.
            case: "one escaped layer, which the first pass decodes",
            target: "/items/a%41b",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/aAb",
            path_parameters: || json!({ "id": "aAb" }),
            status: 200,
        },
        Row {
            case: "two escaped layers, which the second pass decodes to the same text",
            target: "/items/a%2541b",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/aAb",
            path_parameters: || json!({ "id": "aAb" }),
            status: 200,
        },
        Row {
            case: "three escaped layers: the depth is two, so the innermost stays as text",
            target: "/items/a%252541b",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/a%41b",
            path_parameters: || json!({ "id": "a%41b" }),
            status: 200,
        },
        Row {
            case: "an escape the second pass decodes into a percent that begins no escape",
            target: "/items/a%2525b",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/a%b",
            path_parameters: || json!({ "id": "a%b" }),
            status: 200,
        },
        Row {
            case: "an escaped separator, which separates nothing",
            target: "/items/a%2Fb",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/a/b",
            path_parameters: || json!({ "id": "a/b" }),
            status: 200,
        },
        Row {
            case: "an escaped separator spelled in lower case",
            target: "/items/a%2fb",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/a/b",
            path_parameters: || json!({ "id": "a/b" }),
            status: 200,
        },
        Row {
            case: "a literal the api names as a whole route key",
            target: "/items",
            route_key: Some("GET /items"),
            raw_path: "/items",
            path_parameters: || json!({}),
            status: 200,
        },
        Row {
            case: "a trailing slash: one empty last segment, which the variable key captures",
            target: "/items/",
            route_key: Some("GET /items/{id}"),
            raw_path: "/items/",
            path_parameters: || json!({ "id": "" }),
            status: 200,
        },
        Row {
            case: "the greedy remainder over two segments",
            target: "/pets/one/two",
            route_key: Some("GET /pets/{proxy+}"),
            raw_path: "/pets/one/two",
            path_parameters: || json!({ "proxy": "one/two" }),
            status: 200,
        },
        Row {
            case: "a self dot segment, removed before the key is matched",
            target: "/a/./b",
            route_key: Some("GET /a/b"),
            raw_path: "/a/b",
            path_parameters: || json!({}),
            status: 200,
        },
        Row {
            case: "a parent dot segment, which takes the segment above it with it",
            target: "/a/../b",
            route_key: Some("GET /b"),
            raw_path: "/b",
            path_parameters: || json!({}),
            status: 200,
        },
        Row {
            case: "more segments than any key names",
            target: "/items/one/two",
            route_key: None,
            raw_path: "",
            path_parameters: || json!({}),
            status: NOT_FOUND,
        },
        Row {
            case: "an escape the FIRST pass cannot read",
            target: "/items/a%b",
            route_key: None,
            raw_path: "",
            path_parameters: || json!({}),
            status: BAD_REQUEST,
        },
        Row {
            case: "an escape the first pass decodes INTO one the second cannot read",
            target: "/items/a%25b",
            route_key: None,
            raw_path: "",
            path_parameters: || json!({}),
            status: BAD_REQUEST,
        },
        Row {
            case: "an escape the target cut short",
            target: "/items/a%4",
            route_key: None,
            raw_path: "",
            path_parameters: || json!({}),
            status: BAD_REQUEST,
        },
        Row {
            case: "an escape whose digits are not hexadecimal",
            target: "/items/a%zzb",
            route_key: None,
            raw_path: "",
            path_parameters: || json!({}),
            status: BAD_REQUEST,
        },
    ]
}

// The emulator boots a multi-thread runtime (`#[tokio::main]`), and the seam that serves an
// invoke performs the route's integration as a task of the runtime it is served on, so the
// suite drives the router on that same flavour.
#[tokio::test(flavor = "multi_thread")]
async fn an_http_api_decodes_and_normalises_the_target_before_it_selects_a_route() {
    let router = emulator();
    let (host, _api_id) = deployed(&router, "http-api-target-decoding").await;

    for row in rows() {
        let case = row.case;
        let answered = dispatch(&router, "GET", &host, row.target, &[], Bytes::new()).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} from GET {host}{}, got {} with {:?}",
            row.status,
            row.target,
            answered.status,
            answered.text()
        );
        match row.route_key {
            Some(route_key) => {
                assert_eq!(
                    answered.echoed("routeKey"),
                    json!(route_key),
                    "case {case:?}: GET {} must be served by {route_key:?}, got {:?}",
                    row.target,
                    answered.text()
                );
                assert_eq!(
                    answered.echoed("rawPath"),
                    json!(row.raw_path),
                    "case {case:?}: GET {} must report rawPath {:?}, got {:?}",
                    row.target,
                    row.raw_path,
                    answered.text()
                );
                // `requestContext.http.path` is the same path the event's `rawPath` is.
                assert_eq!(
                    answered.echoed("contextPath"),
                    json!(row.raw_path),
                    "case {case:?}: GET {} must report the same path in its requestContext, \
                     got {:?}",
                    row.target,
                    answered.text()
                );
                assert_eq!(
                    answered.echoed("pathParameters"),
                    (row.path_parameters)(),
                    "case {case:?}: GET {} must capture {}, got {:?}",
                    row.target,
                    (row.path_parameters)(),
                    answered.text()
                );
            }
            None => {
                let expected = if row.status == BAD_REQUEST {
                    BAD_REQUEST_MESSAGE
                } else {
                    NOT_FOUND_MESSAGE
                };
                assert_eq!(
                    answered.message(),
                    expected,
                    "case {case:?}: GET {} must be answered the gateway's own {expected:?} \
                     envelope, got {:?}",
                    row.target,
                    answered.text()
                );
                assert_eq!(
                    answered.header("content-type"),
                    Some("application/json"),
                    "case {case:?}: a gateway envelope is JSON, got {:?} in {:?}",
                    answered.header("content-type"),
                    answered.headers
                );
            }
        }
    }
}

/// The QUERY of the target is not part of the path and is mirrored as it arrived: the segments
/// above are read from the path alone, so a query carrying escapes and separators of its own
/// reaches the handler unchanged and selects the same route.
#[tokio::test(flavor = "multi_thread")]
async fn the_query_of_the_target_is_mirrored_and_selects_no_route() {
    let router = emulator();
    let (host, _api_id) = deployed(&router, "http-api-target-query").await;

    // Rows differ along what the query carries: nothing, a pair, a repeated name, an escaped
    // separator, and a value that is itself a path.
    let queries: &[(&str, &str)] = &[
        ("no query at all", ""),
        ("one pair", "colour=red"),
        ("a repeated name", "p=a&p=b"),
        ("an escaped separator in a value", "path=a%2Fb"),
        ("a value that is a path of its own", "next=/items/other"),
    ];
    for (case, query) in queries {
        let target = if query.is_empty() {
            "/items/seven".to_string()
        } else {
            format!("/items/seven?{query}")
        };
        let answered = dispatch(&router, "GET", &host, &target, &[], Bytes::new()).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from GET {host}{target}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.echoed("routeKey"),
            json!("GET /items/{id}"),
            "case {case:?}: the query selects no route, got {:?}",
            answered.text()
        );
        assert_eq!(
            answered.echoed("rawPath"),
            json!("/items/seven"),
            "case {case:?}: the query is no part of the path, got {:?}",
            answered.text()
        );
        assert_eq!(
            answered.echoed("rawQueryString"),
            json!(*query),
            "case {case:?}: the query must be mirrored as it arrived, got {:?}",
            answered.text()
        );
    }
}

/// One request made for the id rule: how it is addressed, and whether the GATEWAY or the
/// integration answers it.
struct IdRow {
    case: &'static str,
    method: &'static str,
    target: &'static str,
    status: u16,
    /// Whether the answer is the integration's, in which case the header must equal the
    /// `requestContext.requestId` the event reported.
    served_by_the_function: bool,
}

const ID_ROWS: &[IdRow] = &[
    IdRow {
        case: "a served request",
        method: "GET",
        target: "/items",
        status: 200,
        served_by_the_function: true,
    },
    IdRow {
        case: "a served request through a variable key",
        method: "GET",
        target: "/items/seven",
        status: 200,
        served_by_the_function: true,
    },
    IdRow {
        // `GET /items` is method-scoped, so it serves no HEAD, and the api holds no `ANY` or
        // `$default` sibling: the gateway answers.
        case: "HEAD on a path only a method-scoped GET key names",
        method: "HEAD",
        target: "/items",
        status: NOT_FOUND,
        served_by_the_function: false,
    },
    IdRow {
        case: "PATCH on that same path",
        method: "PATCH",
        target: "/items",
        status: NOT_FOUND,
        served_by_the_function: false,
    },
    IdRow {
        case: "OPTIONS on that same path, naming no CORS method",
        method: "OPTIONS",
        target: "/items",
        status: NOT_FOUND,
        served_by_the_function: false,
    },
    IdRow {
        case: "a target no decoding pass can read",
        method: "GET",
        target: "/items/a%25b",
        status: BAD_REQUEST,
        served_by_the_function: false,
    },
    IdRow {
        case: "a path no route key of the api names",
        method: "GET",
        target: "/nothing/here",
        status: NOT_FOUND,
        served_by_the_function: false,
    },
];

#[tokio::test(flavor = "multi_thread")]
async fn every_answer_of_an_http_api_names_the_request_it_answered() {
    let router = emulator();
    let (host, _api_id) = deployed(&router, "http-api-gateway-request-id").await;

    let mut reported: Vec<String> = Vec::new();
    for row in ID_ROWS {
        let case = row.case;
        let answered = dispatch(&router, row.method, &host, row.target, &[], Bytes::new()).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} from {} {host}{}, got {} with {:?}",
            row.status,
            row.method,
            row.target,
            answered.status,
            answered.text()
        );
        let stated = answered
            .header(GATEWAY_REQUEST_ID_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: every answer must carry {GATEWAY_REQUEST_ID_HEADER}, got \
                     the headers {:?}",
                    answered.headers
                )
            })
            .to_string();
        assert!(
            !stated.trim().is_empty(),
            "case {case:?}: {GATEWAY_REQUEST_ID_HEADER} must name the request, got {stated:?}"
        );
        if row.served_by_the_function {
            assert_eq!(
                answered.echoed("requestId"),
                json!(stated),
                "case {case:?}: the header must state the id the event reported, got {:?}",
                answered.text()
            );
        }
        reported.push(stated);
    }

    // One id per request: no two of the answers above name the same request.
    let mut unique = reported.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        reported.len(),
        "each request must be named by its own id, got {reported:?}"
    );
}
