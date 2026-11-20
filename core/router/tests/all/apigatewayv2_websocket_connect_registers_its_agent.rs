// SPDX-License-Identifier: CC0-1.0

//! The registration leg of a deployed WEBSOCKET api, through the WHOLE assembled router: the
//! RFC 6455 opening handshake addressed to the socket the api announces, and the `$connect`
//! integration that is performed behind it.
//!
//! The rule under test: **an RFC 6455 opening handshake addressed to a WEBSOCKET api's
//! DEPLOYED stage, at the address that api announces as its `ApiEndpoint`, is answered `101
//! Switching Protocols`, and the `$connect` route's integration is performed before that
//! answer — so the work that integration does is work the services that own it really did. A
//! handshake a step of the api's own stored records refuses is answered that step's DEFINITE
//! status instead (`403` for a stage with no deployment released to it), and that step's work
//! is not performed at all. Either way the handshake reaches an outcome a client can act on,
//! never a status its retry classifier reads as "come back later".**
//!
//! Four crates have to agree for that to hold — API Gateway V2 serves the socket and performs
//! the connection request, Lambda runs the function the `$connect` route fronts, Amazon
//! DynamoDB serves the table that function writes, CloudFormation provisions both — so the
//! suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: the stage name the handshake
//! addresses, the agent it names in its query string, the `Sec-WebSocket-Key` the client
//! offers, and whether the stage has a deployment released to it at all.
//!
//! Each row asserts both halves of its outcome — the status the socket answered, and what
//! DynamoDB holds afterwards, read through its own strongly consistent `GetItem` — so a
//! handshake answered `101` without the connection request having run, and one refused after
//! it ran anyway, both fail here. What the SESSION behind an upgraded handshake carries (the
//! route the api's `RouteSelectionExpression` selects for each frame, and the reply a backend
//! pushes through the `@connections` endpoint) is the API Gateway V2 crate's own
//! `tests/all/websocket_tls_transport.rs` and `tests/all/websocket_transport_data_plane.rs`,
//! which drive it over the same announced address with a WebSocket client library.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, physical_ids_of};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status line RFC 6455 §4.2.2 requires of a COMPLETED opening handshake.
const SWITCHING_PROTOCOLS: u16 = 101;

/// The status API Gateway answers a handshake its own records serve nothing for — here, a
/// stage with no deployment released to it.
const FORBIDDEN: u16 = 403;

/// The field a server's handshake carries its derived accept value in (RFC 6455 §4.2.2), and
/// the pair the specification publishes as its own worked example: the key a client offers,
/// and the accept value a server owes it ("the base64-encoded SHA-1 of the concatenation of
/// the `Sec-WebSocket-Key` with the protocol's GUID", RFC 6455 §1.3). The derivation over the
/// whole range of keys is pinned by the API Gateway V2 crate's
/// `tests/all/websocket_stage_upgrade_counts.rs`; here the published pair is what says the
/// answer is derived from the client's key rather than constant.
///
/// `pub(crate)` because the specification publishes ONE such pair, and a sibling suite that
/// writes its own handshake has to offer the same key to check the same derived answer
/// ([`super::apigatewayv2_announced_address_traffic_is_metered`] reads them from here).
pub(crate) const ACCEPT_FIELD: &str = "sec-websocket-accept";
pub(crate) const RFC_EXAMPLE_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
pub(crate) const RFC_EXAMPLE_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

/// How long one invocation of this handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long to wait between polls of a condition another service owes, and how many polls to
/// make: a minute of looking again, which is generous — nothing here tests how fast a host
/// runs, and the suite shares the machine with every other one.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 1_200;

/// How long a case waits for the socket to answer the handshake it wrote. The connection
/// request is an invocation of a function whose runtime starts on its first call, which on a
/// loaded host takes a while — a span no case here tests, so it is given generous room, and
/// what is waited for is the ANSWER rather than the clock.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// How many bytes of response head a case reads before giving up on the socket: far above the
/// few hundred of a server's handshake.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// The handler every row's `$connect` route fronts: it registers the connection under the
/// agent the handshake named, so that the work the connection request did is readable from the
/// service that owns the table.
const HANDLER_SOURCE: &str = r#"
import os

import boto3


def handler(event, context):
    ctx = event["requestContext"]
    agent = (event.get("queryStringParameters") or {}).get("agent", "unnamed")
    boto3.client("dynamodb").put_item(
        TableName=os.environ["TABLE"],
        Item={
            "id": {"S": agent},
            "connectionId": {"S": ctx["connectionId"]},
            "stage": {"S": ctx["stage"]},
            "routeKey": {"S": ctx["routeKey"]},
        },
    )
    return {"statusCode": 200}
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

/// The emulator as it is ASSEMBLED, on one engine: the socket the api opens, the function its
/// `$connect` route fronts and the table that function writes are all served by this one
/// router.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the handshake carries none, as a
    // WebSocket client sends none.
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

/// One API Gateway V2 control-plane call that has to be accepted with `expected`. The V2
/// endpoint shares the control plane's `apigateway` signing name.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
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
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
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
    .await
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, and
/// answer the configuration it reported then. The span is that service's own, so nothing here
/// sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> Value {
    for _ in 0..POLLS {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        let configuration = reported.json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return configuration;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// Poll `DescribeTable` until DynamoDB first reports the table `ACTIVE` — "you can perform
/// read and write operations only on an ACTIVE table".
async fn wait_until_table_active(router: &MultiServiceRouter, table: &str) {
    for _ in 0..POLLS {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": table })).await;
        if described
            .json()
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str)
            == Some("ACTIVE")
        {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: DynamoDB never reported {table} ACTIVE");
}

/// The item the table holds under `id`, read strongly consistently so that what is read is
/// what the handler wrote.
async fn item_under(router: &MultiServiceRouter, table: &str, id: &str) -> Value {
    let read = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": table,
            "Key": { "id": { "S": id } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "expected HTTP 200 from GetItem on {table:?}, got {} with {:?}",
        read.status,
        read.text()
    );
    read.json().get("Item").cloned().unwrap_or(Value::Null)
}

/// [`item_under`] polled until the table holds an item under `id`: the connection request is
/// performed before the handshake is answered, so the row is owed by the time the answer is
/// read, and this looks again only so that a loaded host does not decide a case. A row that
/// never appears is the failure this names.
async fn registered_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    id: &str,
) -> Value {
    for _ in 0..POLLS {
        let held = item_under(router, table, id).await;
        if !held.is_null() {
            return held;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!(
        "case {case:?}: the $connect integration registered nothing under {id:?} in {table:?}"
    );
}

/// The SAM template the suite deploys: the handler above, and the connections table its
/// `$connect` leg writes, named to the function through the environment variable the transform
/// carries through.
fn template() -> String {
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "Registrar": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "InlineCode": HANDLER_SOURCE,
                    "Environment": { "Variables": { "TABLE": { "Ref": "Connections" } } },
                    "Policies": [
                        { "DynamoDBCrudPolicy": { "TableName": { "Ref": "Connections" } } }
                    ]
                }
            },
            "Connections": {
                "Type": "AWS::Serverless::SimpleTable",
                "Properties": { "PrimaryKey": { "Name": "id", "Type": "String" } }
            }
        }
    })
    .to_string()
}

/// The head the socket answered one handshake with, as the client read it off the wire.
///
/// `pub(crate)` with its fields, because it is what [`answered_handshake_at`] answers, and a
/// sibling suite that writes its handshakes through that reads this head
/// ([`super::apigatewayv2_announced_address_traffic_is_metered`]).
pub(crate) struct Head {
    pub(crate) status: u16,
    /// The status line as written.
    pub(crate) line: String,
    /// Every field, under its lower-cased name.
    pub(crate) fields: BTreeMap<String, String>,
}

impl Head {
    /// The value the answer carries for `name`, read case-insensitively as a field name is.
    pub(crate) fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

/// The authority of the URI `announced` names, and the port in it, having checked the address
/// is the loopback one — so the dial below reaches this machine's own listener and nothing
/// else. Nothing about the address is spelled out here beyond that check: the scheme, the host
/// and the port all come from the value the control plane reported.
///
/// `pub(crate)` for the same reason [`Head`] is: it is part of what [`answered_handshake_at`]
/// does for every suite that writes a handshake through it, so there is one reading of what a
/// WEBSOCKET api announced.
pub(crate) fn announced_loopback(case: &str, announced: &str) -> (String, u16) {
    let (scheme, rest) = announced.split_once("://").unwrap_or_else(|| {
        panic!("case {case:?}: an announced endpoint is an absolute URI, got {announced:?}")
    });
    assert_eq!(
        scheme, "wss",
        "case {case:?}: a WEBSOCKET api is announced with the WebSocket scheme a client dials \
         it by, got {announced:?}"
    );
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: the announced socket names a host:port, got {announced:?}")
    });
    assert_eq!(
        host, "127.0.0.1",
        "case {case:?}: a case dials the emulator's own loopback listener and nothing else, \
         got {announced:?}"
    );
    let port = port.parse().unwrap_or_else(|_| {
        panic!("case {case:?}: the announced port must parse, got {announced:?}")
    });
    (authority.to_string(), port)
}

/// Write one RFC 6455 opening handshake to the announced socket's `/{stage}?agent={agent}` and
/// read the head it is answered with.
///
/// The request bytes are written and the answer's head read directly, rather than through a
/// WebSocket client library, because what this suite pins is that HEAD: the status a client's
/// retry classifier reads. The session behind an upgraded handshake is driven with a client
/// library by the suites named in this module's docs.
async fn answered_handshake(
    case: &str,
    announced: &str,
    stage: &str,
    agent: &str,
    key: &str,
) -> Head {
    answered_handshake_at(case, announced, &format!("/{stage}?agent={agent}"), key).await
}

/// [`answered_handshake`] against any `path` of the announced socket: the stage a client
/// addresses, and whatever query string it carries.
///
/// `pub(crate)` because the bytes a client writes are RFC 6455's rather than this suite's, and a
/// second copy of them is a second statement of what a client sends
/// ([`super::apigatewayv2_announced_address_traffic_is_metered`] writes its own handshakes
/// through this).
pub(crate) async fn answered_handshake_at(
    case: &str,
    announced: &str,
    path: &str,
    key: &str,
) -> Head {
    let (authority, port) = announced_loopback(case, announced);
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the announced socket {authority:?} must accept a connection, \
                 got {error}"
            )
        });
    let mut request = format!("GET {path} HTTP/1.1\r\n");
    for (name, value) in [
        ("Host", authority.as_str()),
        ("Connection", "Upgrade"),
        ("Upgrade", "websocket"),
        ("Sec-WebSocket-Key", key),
        ("Sec-WebSocket-Version", "13"),
    ] {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the handshake must be written, got {error}")
        });
    let head = tokio::time::timeout(ANSWER_DEADLINE, read_head(&mut stream))
        .await
        .unwrap_or_else(|_elapsed| {
            panic!("case {case:?}: the handshake must be answered a head")
        })
        .unwrap_or_else(|| panic!("case {case:?}: the socket answered no readable head"));
    let (line, fields) = head.split_once("\r\n").unwrap_or((head.as_str(), ""));
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: no status code in {line:?}"));
    let fields = fields
        .split("\r\n")
        .filter_map(|field| field.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Head {
        status,
        line: line.to_string(),
        fields,
    }
}

/// The bytes of the response head, up to and including the blank line that ends it (RFC 9112
/// §2.1), or `None` for a socket that answered none within [`MAX_HEAD_BYTES`].
///
/// `pub(crate)` for the same reason [`Head`] is: it is where [`answered_handshake_at`] stops
/// reading an answer — a completed handshake is followed by frames rather than by a body — so
/// every suite that writes a handshake through it stops at the same place.
pub(crate) async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    while head.len() < MAX_HEAD_BYTES {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => head.extend_from_slice(&chunk[..read]),
        }
        if let Some(end) = head
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            return String::from_utf8(head[..end].to_vec()).ok();
        }
    }
    None
}

/// One row: the api a client dials, the handshake it writes, and the outcome that handshake
/// owes.
struct Row {
    case: &'static str,
    /// The stage the api creates and the handshake addresses.
    stage: &'static str,
    /// The agent the handshake names in its query string, under which the `$connect`
    /// integration registers the connection.
    agent: &'static str,
    /// The `Sec-WebSocket-Key` the client offers: "a base64-encoded value that, when decoded,
    /// is 16 bytes in length" (RFC 6455 §4.1).
    key: &'static str,
    /// The accept value RFC 6455 §1.3 PUBLISHES for that key, where it publishes one.
    published_accept: Option<&'static str>,
    /// Whether a deployment is released to the stage.
    deploy: bool,
    /// The status the handshake owes, and whether the connection request ran for it. Both
    /// values are DEFINITE: neither is a status a client's retry classifier reads as
    /// transient.
    expected: u16,
    registers: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a deployed stage, with the nonce RFC 6455 §1.3 publishes as its example",
        stage: "prod",
        agent: "agent-one",
        key: RFC_EXAMPLE_KEY,
        published_accept: Some(RFC_EXAMPLE_ACCEPT),
        deploy: true,
        expected: SWITCHING_PROTOCOLS,
        registers: true,
    },
    Row {
        case: "another deployed stage, another agent and another nonce",
        stage: "preview-2",
        agent: "second-agent",
        key: "AQIDBAUGBwgJCgsMDQ4PEA==",
        published_accept: None,
        deploy: true,
        expected: SWITCHING_PROTOCOLS,
        registers: true,
    },
    Row {
        case: "a stage with no deployment released to it",
        stage: "unreleased",
        agent: "agent-three",
        key: "EBAPDgwLCgkIBwYFBAMCAQ==",
        published_accept: None,
        deploy: false,
        expected: FORBIDDEN,
        registers: false,
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployed_websocket_stage_upgrades_the_handshake_and_runs_its_connect_integration() {
    let router = emulator();

    // 1. The function every row's `$connect` route fronts, and the table it writes.
    let stack = "websocket-agent-registration";
    let body = template();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and
            // the role the transform synthesizes for the function with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let resources =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    let physical = physical_ids_of(&resources.text());
    let function_name = physical
        .get("Registrar")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no physical id for Registrar"));
    let table = physical
        .get("Connections")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no physical id for Connections"));
    let configuration = wait_until_active(&router, &function_name).await;
    wait_until_table_active(&router, &table).await;
    let function_arn = member("GetFunctionConfiguration", &configuration, "FunctionArn");

    // The grant that admits an invocation from API Gateway, without which the gateway makes
    // none.
    let granted = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function_name}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status, 201,
        "setup: expected HTTP 201 from AddPermission, got {} with {:?}",
        granted.status,
        granted.text()
    );

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;

        // 2. The api: a `$connect` route bound to an AWS_PROXY integration on that function,
        //    and the stage this row's handshake addresses.
        let api_record = apigw(
            &router,
            case,
            "POST",
            "/v2/apis",
            Some(json!({
                "name": format!("agent-registry-{index}"),
                "protocolType": "WEBSOCKET",
                "routeSelectionExpression": "$request.body.action",
            })),
            201,
        )
        .await;
        let api_id = member("CreateApi", &api_record, "apiId");
        let announced = member("CreateApi", &api_record, "apiEndpoint");
        let integration = apigw(
            &router,
            case,
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": function_arn,
                "integrationMethod": "POST",
            })),
            201,
        )
        .await;
        let integration_id = member("CreateIntegration", &integration, "integrationId");
        for route_key in ["$connect", "$default", "$disconnect"] {
            apigw(
                &router,
                case,
                "POST",
                &format!("/v2/apis/{api_id}/routes"),
                Some(json!({
                    "routeKey": route_key,
                    "target": format!("integrations/{integration_id}"),
                })),
                201,
            )
            .await;
        }
        apigw(
            &router,
            case,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({ "stageName": row.stage })),
            201,
        )
        .await;
        if row.deploy {
            apigw(
                &router,
                case,
                "POST",
                &format!("/v2/apis/{api_id}/deployments"),
                Some(json!({ "stageName": row.stage })),
                201,
            )
            .await;
        }

        // 3. The handshake, at the address the api announces, with the stage appended.
        let head = answered_handshake(case, &announced, row.stage, row.agent, row.key).await;
        assert_eq!(
            head.status, row.expected,
            "case {case:?}: expected the handshake to be answered {}, got the head {:?}",
            row.expected, head.line
        );
        if row.expected == SWITCHING_PROTOCOLS {
            let accept = head.field(ACCEPT_FIELD).unwrap_or_default().to_string();
            assert!(
                !accept.is_empty(),
                "case {case:?}: a completed handshake carries the accept value the client's \
                 key derives, got the fields {:?}",
                head.fields
            );
            match row.published_accept {
                // The one pair RFC 6455 §1.3 publishes: this key owes exactly this value.
                Some(published) => assert_eq!(
                    accept, published,
                    "case {case:?}: expected the accept value RFC 6455 §1.3 publishes for the \
                     key {:?}, got {accept:?}",
                    row.key
                ),
                // A key the specification publishes no answer for still cannot be answered the
                // published one: the value is derived from the key the client offered, so a
                // server answering one constant would be caught here.
                None => assert_ne!(
                    accept, RFC_EXAMPLE_ACCEPT,
                    "case {case:?}: the accept value is derived from the key the client \
                     offered ({:?}), so it cannot be the one another key owes, got {accept:?}",
                    row.key
                ),
            }
        }

        // 4. What the connection request DID, read from the service that owns the table: the
        //    row the `$connect` integration wrote for an admitted handshake, naming this
        //    connection, this stage and the route that served the call — and no row at all
        //    where the handshake was refused before any integration ran.
        if row.registers {
            let held = registered_item(&router, case, &table, row.agent).await;
            let value = |name: &str| {
                held.get(name)
                    .and_then(|value| value.get("S"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            assert!(
                value("connectionId").is_some_and(|id| !id.is_empty()),
                "case {case:?}: expected the $connect integration to register {:?} in \
                 {table:?} with the id of the connection it was admitting, got {held}",
                row.agent
            );
            assert_eq!(
                value("stage").as_deref(),
                Some(row.stage),
                "case {case:?}: expected the registered row to name the stage that admitted \
                 the connection, got {held}"
            );
            assert_eq!(
                value("routeKey").as_deref(),
                Some("$connect"),
                "case {case:?}: expected the registered row to name the route that served the \
                 connection request, got {held}"
            );
        } else {
            let held = item_under(&router, &table, row.agent).await;
            assert_eq!(
                held,
                Value::Null,
                "case {case:?}: a refused handshake performs no connection request, so {:?} \
                 must be registered nowhere, got {held}",
                row.agent
            );
        }
    }
}
