// SPDX-License-Identifier: CC0-1.0

//! The EVENT a route materialised by a SAM `HttpApi` event hands its function, as the
//! `$default` stage of the HTTP api the macro adds serves it.
//!
//! The rule under test: **a request the `$default` stage of an HTTP api the
//! `AWS::Serverless-2016-10-31` transform materialised matches is answered by the matched
//! route's `AWS_PROXY` integration running the function on the documented payload-format-`2.0`
//! proxy event, every member of which is computed from the request that arrived — `routeKey`
//! the matched route's key, `rawPath` the path the stage was addressed at, `rawQueryString` the
//! query exactly as it arrived, `queryStringParameters` and `pathParameters` and `cookies`
//! present only when the request carries something for them, `headers` the request's own fields
//! with the cookies lifted out, `body` with `isBase64Encoded`, and a `requestContext` naming the
//! api, the stage, the route key, the method, the path and the caller's address — while the run
//! itself takes the configuration the template's `Globals.Function` block gave the generated
//! function, which is what the Lambda context reports.** Three crates have to agree for that to
//! hold (CloudFormation expands the macro and provisions what it emits, API Gateway v2 resolves
//! the stage and the route and performs the proxy integration, AWS Lambda runs the package), so
//! the suite lives here and drives the emulator's own assembled service set.
//!
//! `crate::cloudformation_sam_http_api_event_serves_its_route` owns the half of that rule a
//! client sees from OUTSIDE — which status, which body and which `Access-Control-*` header each
//! request is answered with, over declared and implicit apis — and
//! `crate::cloudformation_sam_http_api_definitions` the control-plane half of the expansion,
//! which is where the route keys, the `AWS_PROXY` integrations and the `$default` stage the
//! macro emits are read back. What this suite adds is the ENVELOPE the function is actually run
//! on: the handler answers with the event it received, so every member above is read from the
//! run rather than from the api's records.
//!
//! The rows differ along what the rule generalises over: the event's method (`GET`, `POST`, the
//! catch-all `ANY`), whether its path is literal, names a variable or ends in a greedy segment,
//! how many segments such a path consumes, whether the request carries no query, one parameter
//! or a parameter stated twice, whether it carries cookies, whether it carries a body, and
//! whether a proxy in front of it states the original caller's address. Every expected value is
//! computed here from what the template declares and what the request carried — never read back
//! out of the answer under test.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical id of the HTTP api the macro adds for the `HttpApi` events that name none
/// ("AWS SAM creates … an implicit API with the logical ID `ServerlessHttpApi`"), and the stage
/// such an api is deployed to.
const IMPLICIT_API: &str = "ServerlessHttpApi";
const DEFAULT_STAGE: &str = "$default";

/// The `version` member of the payload format an `HttpApi` event's integration defaults to
/// (`Integration$PayloadFormatVersion`, whose values for an HTTP api are `1.0` and `2.0`).
const PAYLOAD_FORMAT: &str = "2.0";

/// The one origin the template's `Globals.HttpApi.CorsConfiguration` admits. No row here sends
/// an `Origin`: which `Access-Control-*` header an answer carries is the sibling suite's rule,
/// and the configuration stands in this template because the `sam deploy` this rule is stated
/// about carries one.
const ALLOWED_ORIGIN: &str = "https://sam.example.test";

/// The runtime and handler the template's `Globals.Function` block states, and the memory size
/// it states — inside `MemorySize`'s published range, `@range` min 128, max 32768
/// (`smithy_query(query="shape", shape="MemorySize")` on the Lambda model).
const GLOBAL_RUNTIME: &str = "python3.13";
const GLOBAL_HANDLER: &str = "index.handler";
const GLOBAL_MEMORY_SIZE: i64 = 192;

/// The protocol the event reports the request arrived over: the emulator's front door serves
/// HTTP/1.1, which is what these requests arrive on.
const REQUEST_PROTOCOL: &str = "HTTP/1.1";

/// The address the event reports as the caller's for a request that carries nothing stating one
/// — the loopback address a client of this emulator reaches it from.
const LOOPBACK: &str = "127.0.0.1";

/// The header a proxy states the original caller's address in, whose FIRST entry is the
/// client's, and the one a client carries its cookies in (RFC 6265 §5.4), which a `2.0` event
/// lifts into its own list.
const FORWARDED_FOR: &str = "x-forwarded-for";
const COOKIE: &str = "cookie";

/// A header each row sends a value of its own in, so the `headers` member is read as the
/// request's own fields rather than as a map that happens to carry the right names.
const ROW_HEADER: &str = "x-sam-row";

/// How long to wait between polls of a clock-owed transition, and how long to keep looking.
const POLL: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(120);

/// The handler the function is deployed with, inline in the template as `sam deploy` carries
/// it: it answers with the EVENT it was called on and the two members of its own context that
/// say which function ran and under which memory size, so the whole envelope is read from the
/// run.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {
                "event": event,
                "functionName": context.function_name,
                "memoryLimitInMb": context.memory_limit_in_mb,
            }
        ),
    }
"#;

/// One answered request: its status and its body verbatim.
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

/// The emulator as it is ASSEMBLED, on one engine: the stack's api and function are held by the
/// same router the invokes below reach.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published endpoint
    // carries none, as a client of a public HTTP api sends none.
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

/// `GetFunctionConfiguration` on one function, as a client of the declared name reads it.
async fn function_configuration(router: &MultiServiceRouter, name: &str) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("lambda.{REGION}.amazonaws.com"),
        &format!("/2015-03-31/functions/{name}/configuration"),
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::new(),
    )
    .await
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, and answer
/// the configuration it reported then. The span is that service's own, so nothing here sleeps a
/// fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let reported = function_configuration(router, name).await;
        let configuration = reported.json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return configuration;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda never reported {name} Active; last answer {:?}",
            reported.text()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One `HttpApi` event of the function, naming no api of its own — the event the macro adds
/// its own api for.
fn http_event(path: &str, method: &str) -> Value {
    json!({ "Type": "HttpApi", "Properties": { "Path": path, "Method": method } })
}

/// The template the suite deploys: one `AWS::Serverless::Function` whose three `HttpApi` events
/// route through the api the macro adds, and the `Globals` block that stands in for what the
/// declaration does not state.
fn template(function: &str) -> String {
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Globals": {
            "Function": {
                "Runtime": GLOBAL_RUNTIME,
                "Handler": GLOBAL_HANDLER,
                "MemorySize": GLOBAL_MEMORY_SIZE
            },
            "HttpApi": {
                "CorsConfiguration": {
                    "AllowOrigins": [ALLOWED_ORIGIN],
                    "AllowMethods": ["GET", "POST"]
                }
            }
        },
        "Resources": {
            "Handler": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "FunctionName": function,
                    "InlineCode": HANDLER_SOURCE,
                    "Events": {
                        "Hello": http_event("/hello", "get"),
                        "Order": http_event("/orders/{id}", "post"),
                        "Rest": http_event("/any/{proxy+}", "any")
                    }
                }
            }
        },
        "Outputs": {
            "Endpoint": { "Value": { "Fn::GetAtt": [IMPLICIT_API, "ApiEndpoint"] } }
        }
    })
    .to_string()
}

/// One row: the request a client makes on the published endpoint, and the envelope the rule
/// owes the function it reaches.
struct Row {
    case: &'static str,
    /// The method and the path within the stage the client asks for, and the query string it
    /// carries with no leading `?` — empty for a request carrying none.
    method: &'static str,
    path: &'static str,
    query: &'static str,
    /// The `Cookie` field line the request carries, or `None` for one carrying none.
    cookie: Option<&'static str>,
    /// The `X-Forwarded-For` field line a proxy in front of the request stated, or `None`.
    forwarded_for: Option<&'static str>,
    /// The request's body, empty for one carrying none.
    body: &'static str,
    /// The `RouteKey` of the route that must serve it.
    route_key: &'static str,
    /// `queryStringParameters` the envelope must carry, as name/value pairs — empty where the
    /// member must be absent altogether.
    query_parameters: &'static [(&'static str, &'static str)],
    /// `pathParameters`, likewise.
    path_parameters: &'static [(&'static str, &'static str)],
    /// `cookies`, likewise.
    cookies: &'static [&'static str],
    /// The address `requestContext.http.sourceIp` must report: the CONNECTION the gateway
    /// read the request from, which for this emulator's front door is the loopback address
    /// — never an address the request CLAIMED in a forwarded header, which is a value any
    /// client can write and which the gateway therefore only APPENDS its own view of the peer
    /// to.
    source_ip: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a literal GET route, with no query, no cookie and no body",
        method: "GET",
        path: "/hello",
        query: "",
        cookie: None,
        forwarded_for: None,
        body: "",
        route_key: "GET /hello",
        query_parameters: &[],
        path_parameters: &[],
        cookies: &[],
        source_ip: LOOPBACK,
    },
    Row {
        case: "the same route carrying one query parameter",
        method: "GET",
        path: "/hello",
        query: "greeting=hi",
        cookie: None,
        forwarded_for: None,
        body: "",
        route_key: "GET /hello",
        query_parameters: &[("greeting", "hi")],
        path_parameters: &[],
        cookies: &[],
        source_ip: LOOPBACK,
    },
    Row {
        case: "the same route carrying a parameter the query states twice, which the format \
               reports joined",
        method: "GET",
        path: "/hello",
        query: "tag=a&tag=b&page=2",
        cookie: None,
        forwarded_for: None,
        body: "",
        route_key: "GET /hello",
        // A parameter sent twice is reported as ONE comma-separated value, which is the
        // single-valued form a `2.0` event's `queryStringParameters` states a field in.
        query_parameters: &[("page", "2"), ("tag", "a,b")],
        path_parameters: &[],
        cookies: &[],
        source_ip: LOOPBACK,
    },
    Row {
        case: "a POST route whose path names a variable the request fills, carrying a text body",
        method: "POST",
        path: "/orders/7",
        query: "",
        cookie: None,
        forwarded_for: None,
        body: r#"{"item":"book"}"#,
        route_key: "POST /orders/{id}",
        query_parameters: &[],
        path_parameters: &[("id", "7")],
        cookies: &[],
        source_ip: LOOPBACK,
    },
    Row {
        case: "the same POST route behind a proxy that states the caller's address, with a \
               query beside the path variable",
        method: "POST",
        path: "/orders/42",
        query: "note=gift",
        cookie: None,
        forwarded_for: Some("203.0.113.9, 198.51.100.4"),
        body: r#"{"item":"map"}"#,
        route_key: "POST /orders/{id}",
        query_parameters: &[("note", "gift")],
        path_parameters: &[("id", "42")],
        cookies: &[],
        // The claimed chain is relayed and the gateway's own view of the peer is appended to
        // it, but the address the envelope REPORTS is that peer: the connection this request
        // was read from.
        source_ip: LOOPBACK,
    },
    Row {
        case: "an ANY route whose path ends in a greedy segment the request fills with two, \
               carrying two cookies",
        method: "GET",
        path: "/any/deep/leaf",
        query: "",
        cookie: Some("session=abc; theme=dark"),
        forwarded_for: None,
        body: "",
        route_key: "ANY /any/{proxy+}",
        query_parameters: &[],
        path_parameters: &[("proxy", "deep/leaf")],
        cookies: &["session=abc", "theme=dark"],
        source_ip: LOOPBACK,
    },
];

/// The member of `event` the dotted `path` names, or `None` for one the event does not carry.
fn member(event: &Value, path: &str) -> Option<Value> {
    let mut found = event;
    for name in path.split('.') {
        found = found.get(name)?;
    }
    Some(found.clone())
}

/// `pairs` as the single-valued map a `2.0` event states such a collection in, or `None` for an
/// empty one — a member the request carries nothing for is LEFT OFF the event rather than sent
/// as an empty object.
fn map_or_absent(pairs: &[(&str, &str)]) -> Option<Value> {
    (!pairs.is_empty()).then(|| {
        Value::Object(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), json!(value)))
                .collect(),
        )
    })
}

/// `values` as the list a `2.0` event states its cookies in, or `None` for an empty one.
fn list_or_absent(values: &[&str]) -> Option<Value> {
    (!values.is_empty()).then(|| json!(values))
}

/// An announced `ApiEndpoint` split into the authority a request is addressed to and the path
/// prefix it carries: this emulator announces its own address with the api's id in the path,
/// while the form `Api$ApiEndpoint` documents carries the id in the host and no path at all. A
/// client dials whichever it was handed, which is what this suite does.
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

/// The account segment of a Lambda function's ARN
/// (`arn:<partition>:lambda:<region>:<account>:function:<name>`), which is the account the
/// stack's resources belong to and so the one the event's `requestContext` reports.
fn account_of(function_arn: &str) -> String {
    function_arn
        .split(':')
        .nth(4)
        .unwrap_or_else(|| panic!("setup: expected a function ARN, got {function_arn:?}"))
        .to_string()
}

// The emulator boots a multi-thread runtime (`#[tokio::main]`), and the seam that serves an
// invoke performs the route's integration as a task of the runtime it is served on, so the
// suite drives the router on that same flavour.
#[tokio::test(flavor = "multi_thread")]
async fn a_sam_http_api_route_runs_its_function_on_the_requests_own_two_zero_envelope() {
    let router = emulator();

    // 1. `sam deploy` is CreateStack of the template with the macro acknowledged.
    let stack = "sam-httpapi-proxy-envelope";
    let function = format!("{stack}-handler");
    let body = template(&function);
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            // The role the transform synthesizes for the function is acknowledged with
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
        let events = cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events.text(), "ResourceStatusReason")
    };
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described} with resource \
         failures {failures:?}"
    );

    // 2. The api the macro added, by the physical id the stack reports for it, and the endpoint
    // the stack published for it — which is what a client is handed.
    let resources =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    let physical_ids = physical_ids_of(&resources.text());
    let api_id = physical_ids
        .get(IMPLICIT_API)
        .cloned()
        .unwrap_or_else(|| panic!("setup: no {IMPLICIT_API} resource in {}", resources.text()));
    let outputs = outputs_of(&described);
    let announced = outputs
        .get("Endpoint")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no Endpoint output in {described}"));
    assert!(
        !announced.is_empty(),
        "setup: the stack's Endpoint output resolved to nothing in {described}"
    );
    let (authority, prefix) = addressed(&announced);

    // 3. The function the macro generated, once Lambda reports it ready to run, and the account
    // its ARN names.
    let configuration = wait_until_active(&router, &function).await;
    let function_arn = configuration
        .get("FunctionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: no FunctionArn in {configuration}"));
    let account = account_of(function_arn);

    // 4. Every row, unsigned, on the endpoint the stack announced — as a client of a published
    // HTTP api calls it.
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let row_marker = index.to_string();
        let addressed_path = if row.query.is_empty() {
            format!("{prefix}{}", row.path)
        } else {
            format!("{prefix}{}?{}", row.path, row.query)
        };
        let mut headers: Vec<(&str, &str)> = vec![(ROW_HEADER, row_marker.as_str())];
        if let Some(cookie) = row.cookie {
            headers.push((COOKIE, cookie));
        }
        if let Some(forwarded) = row.forwarded_for {
            headers.push((FORWARDED_FOR, forwarded));
        }
        let served = dispatch(
            &router,
            row.method,
            &authority,
            &addressed_path,
            &headers,
            Bytes::from(row.body),
        )
        .await;
        assert_eq!(
            served.status,
            200,
            "case {case:?}: expected HTTP 200 from {} {authority}{addressed_path}, got {} with \
             {:?}",
            row.method,
            served.status,
            served.text()
        );
        let answered = served.json();

        // The run itself: the function the macro generated, under the memory size the `Globals`
        // block gave it. The context reports the size as the text the runtime reads it from the
        // invocation's environment as.
        let ran_as = answered.get("functionName").and_then(Value::as_str);
        assert_eq!(
            ran_as,
            Some(function.as_str()),
            "case {case:?}: expected the route's own function {function:?} to have run, got \
             {ran_as:?} in {answered}"
        );
        let memory = answered.get("memoryLimitInMb").and_then(Value::as_str);
        let expected_memory = GLOBAL_MEMORY_SIZE.to_string();
        assert_eq!(
            memory,
            Some(expected_memory.as_str()),
            "case {case:?}: expected the Globals memory size {expected_memory:?}, got {memory:?} \
             in {answered}"
        );

        // The envelope: every member computed here from the request that arrived.
        let event = answered
            .get("event")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no event in {answered}"));
        let expected: Vec<(&str, Option<Value>)> = vec![
            ("version", Some(json!(PAYLOAD_FORMAT))),
            ("routeKey", Some(json!(row.route_key))),
            ("rawPath", Some(json!(row.path))),
            ("rawQueryString", Some(json!(row.query))),
            ("queryStringParameters", map_or_absent(row.query_parameters)),
            ("pathParameters", map_or_absent(row.path_parameters)),
            ("cookies", list_or_absent(row.cookies)),
            ("body", (!row.body.is_empty()).then(|| json!(row.body))),
            ("isBase64Encoded", Some(json!(false))),
            ("requestContext.accountId", Some(json!(account))),
            ("requestContext.apiId", Some(json!(api_id))),
            ("requestContext.stage", Some(json!(DEFAULT_STAGE))),
            ("requestContext.routeKey", Some(json!(row.route_key))),
            ("requestContext.http.method", Some(json!(row.method))),
            ("requestContext.http.path", Some(json!(row.path))),
            (
                "requestContext.http.protocol",
                Some(json!(REQUEST_PROTOCOL)),
            ),
            ("requestContext.http.sourceIp", Some(json!(row.source_ip))),
        ];
        for (path, wanted) in expected {
            let held = member(&event, path);
            assert_eq!(
                held, wanted,
                "case {case:?}: expected the event's {path} to be {wanted:?}, got {held:?} in \
                 {event}"
            );
        }

        // `headers` is the request's own field set: the value this row sent is there, and the
        // cookies are reported in `cookies` instead of being left in the map.
        let sent_header = member(&event, &format!("headers.{ROW_HEADER}"));
        assert_eq!(
            sent_header,
            Some(json!(row_marker)),
            "case {case:?}: expected the event's headers.{ROW_HEADER} to be the value the \
             request sent, {row_marker:?}, got {sent_header:?} in {event}"
        );
        let cookie_header = member(&event, &format!("headers.{COOKIE}"));
        assert_eq!(
            cookie_header, None,
            "case {case:?}: expected the cookies to be lifted out of the event's headers, got \
             headers.{COOKIE} = {cookie_header:?} in {event}"
        );
    }
}

/// One row of the collection-member table below: the case, the pairs the request carried, and
/// the member the envelope must state for them.
type PairsRow = (
    &'static str,
    &'static [(&'static str, &'static str)],
    Option<Value>,
);

/// The readers above answer for the two shapes a `2.0` event states such a collection in: the
/// member is there with every pair when the request carried something, and absent altogether
/// when it carried nothing.
#[test]
fn the_expected_members_are_absent_exactly_when_the_request_carried_nothing() {
    // (case, the pairs the request carried, the member the envelope must state)
    let rows: [PairsRow; 3] = [
        ("nothing at all", &[], None),
        ("one pair", &[("a", "1")], Some(json!({"a": "1"}))),
        (
            "two pairs",
            &[("a", "1"), ("b", "2")],
            Some(json!({"a": "1", "b": "2"})),
        ),
    ];
    for (case, pairs, expected) in rows {
        let built = map_or_absent(pairs);
        assert_eq!(
            built, expected,
            "case {case:?}: expected {expected:?}, got {built:?}"
        );
    }

    let cookies: [(&str, &[&str], Option<Value>); 2] = [
        ("no cookie at all", &[], None),
        ("two cookies", &["a=1", "b=2"], Some(json!(["a=1", "b=2"]))),
    ];
    for (case, values, expected) in cookies {
        let built = list_or_absent(values);
        assert_eq!(
            built, expected,
            "case {case:?}: expected {expected:?}, got {built:?}"
        );
    }
}

/// The member reader walks a dotted path and answers nothing for a member the event does not
/// carry, which is what makes an absent member assert as absent rather than as a missing parent.
#[test]
fn the_member_reader_walks_a_dotted_path() {
    let event = json!({
        "version": "2.0",
        "requestContext": { "http": { "method": "GET" }, "stage": "$default" }
    });
    // (case, the path, the member it names)
    let rows: [(&str, &str, Option<Value>); 5] = [
        ("a root member", "version", Some(json!("2.0"))),
        (
            "a nested member",
            "requestContext.stage",
            Some(json!("$default")),
        ),
        (
            "a member two levels in",
            "requestContext.http.method",
            Some(json!("GET")),
        ),
        ("a root member the event omits", "cookies", None),
        (
            "a nested member the event omits",
            "requestContext.authorizer.jwt",
            None,
        ),
    ];
    for (case, path, expected) in rows {
        let held = member(&event, path);
        assert_eq!(
            held, expected,
            "case {case:?}: expected {expected:?}, got {held:?}"
        );
    }
}

/// The ARN reader names the account segment, over the ARN forms a function's own carries.
#[test]
fn the_arn_reader_names_the_account_segment() {
    // (case, the ARN, the account it names)
    let rows: [(&str, &str, &str); 3] = [
        (
            "a function ARN",
            "arn:aws:lambda:us-east-1:000000000000:function:handler",
            "000000000000",
        ),
        (
            "a qualified function ARN",
            "arn:aws:lambda:eu-west-1:123456789012:function:handler:live",
            "123456789012",
        ),
        (
            "another partition",
            "arn:aws-us-gov:lambda:us-gov-west-1:210987654321:function:handler",
            "210987654321",
        ),
    ];
    for (case, arn, expected) in rows {
        let read = account_of(arn);
        assert_eq!(
            read, expected,
            "case {case:?}: expected {expected:?}, got {read:?}"
        );
    }
}

/// The endpoint splitter answers the authority and the path prefix of each form an
/// `ApiEndpoint` is announced in, so a row dials what the stack published.
#[test]
fn the_endpoint_splitter_answers_the_authority_and_the_prefix() {
    // (case, the announced endpoint, the authority, the prefix)
    let rows: [(&str, &str, &str, &str); 4] = [
        (
            "the documented host-style form",
            "https://abc123.execute-api.us-east-1.amazonaws.com",
            "abc123.execute-api.us-east-1.amazonaws.com",
            "",
        ),
        (
            "the same with a trailing slash",
            "https://abc123.execute-api.us-east-1.amazonaws.com/",
            "abc123.execute-api.us-east-1.amazonaws.com",
            "",
        ),
        (
            "this emulator's own address with the api id in the path",
            "https://127.0.0.1:9297/_aws/execute-api/abc123",
            "127.0.0.1:9297",
            "/_aws/execute-api/abc123",
        ),
        (
            "a bracketed IPv6 literal",
            "https://[::1]:9297/_aws/execute-api/abc123",
            "[::1]:9297",
            "/_aws/execute-api/abc123",
        ),
    ];
    for (case, announced, authority, prefix) in rows {
        let split = addressed(announced);
        assert_eq!(
            split,
            (authority.to_string(), prefix.to_string()),
            "case {case:?}: expected ({authority:?}, {prefix:?}), got {split:?}"
        );
    }
}
