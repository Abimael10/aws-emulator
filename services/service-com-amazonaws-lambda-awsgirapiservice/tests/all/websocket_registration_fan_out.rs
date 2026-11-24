// SPDX-License-Identifier: CC0-1.0

//! A whole REGISTRATION / FAN-OUT session of a deployed WEBSOCKET api, driven over real sockets:
//! clients register themselves with a frame, the backend answers each one through
//! `@connections`, and every later registration is announced to the clients that are still
//! there — including the pass where one of them has departed.
//!
//! > **RULE: a session of a deployed WEBSOCKET stage reaches a DEFINITE outcome on every leg.
//! > Each frame is dispatched to the route the api's `RouteSelectionExpression` selects for it —
//! > `$default` where that path reaches a value no route key matches — and that route's
//! > integration RUNS, with `requestContext` naming the connection, the route and the host the
//! > client addressed, so the callback URL the backend composes from `domainName` and `stage` is
//! > an address its own `@connections` client is served at: what it posts there is delivered to
//! > that connection as a frame the client reads back. A `PostToConnection` naming a connection
//! > whose `$disconnect` has already run is refused `410 GoneException` instead — the one answer
//! > that tells a sender to stop writing to that connection — so a backend fanning out over its
//! > OWN record of connections prunes exactly the row it was refused for and still delivers to
//! > every connection that remains, however many of those there are. And a backend that composes
//! > no callback address at all still completes the writes its run makes, while nothing is
//! > delivered to the client: the frames a connection reads are the ones something POSTED to it,
//! > never one the gateway invented. No leg is left pending — a client that sent a frame is
//! > answered, refused, or told nothing because nothing was pushed, and never kept waiting.**
//!
//! WHY the whole session and not one leg: an application built on `@connections` fan-out is a
//! loop over a table of connections, and it learns what to do with each row only from the answer
//! its own push earns. A push that hangs stalls the loop, so the clients after that row are never
//! notified; a push answered as if it were delivered leaves a dead row in the table forever; and
//! a refusal of the wrong class makes the backend drop a live subscriber it had just admitted.
//!
//! The backend is this crate's: AWS Lambda runs the function behind every route, its in-process
//! SDK seam carries the `@connections` pushes and the table writes that function makes, and it
//! delivers the run's records to CloudWatch Logs. The services it is driven against are mounted
//! beside it on ONE engine — API Gateway V2 serves the sockets, selects the route and performs
//! the integration, the API Gateway Management API answers each push, Amazon DynamoDB holds the
//! backend's own record of connections, Amazon CloudWatch Logs holds the records of the runs, and
//! IAM and the token service answer for what the execution role those calls are made under may
//! do.
//!
//! Nothing asserted below is a value this emulator fills in for the test: every claim is on a
//! frame a FUNCTION pushed, on the row it wrote or deleted, or on what its own SDK was answered
//! for each id of the fan-out, so a leg that never ran reads as a missing frame or a missing row
//! rather than as something that merely arrived.
//!
//! The rows span what the rule generalises over: the number of live connections one fan-out
//! addresses (one, and two), whether a departed connection's row is among the ids it addresses
//! (so a pass that prunes and a pass that prunes nothing are both driven), and the payload each
//! registration carries. The two registration frames also span the DISPATCH: one selects a route
//! key the api holds and the other reaches `$default`, and each reply names the route that served
//! it.
//!
//! What the neighbouring suites own and this one does not restate: the frame-to-route selection
//! over every expression form, and the handshake's own statuses for each state of the api's
//! records, are the API Gateway V2 crate's `websocket_lambda_routes.rs` and
//! `websocket_transport_data_plane.rs`; the one round trip of a single connection, and what the
//! close leaves behind, is this crate's `websocket_backend_response_lifecycle.rs`; and what each
//! `@connections` operation answers for every FORM of id is
//! `core/router/tests/all/apigatewaymanagementapi_minted_connection_endpoint.rs`. What is added
//! here is the SESSION of several connections at once: a fan-out that meets a departed connection
//! half way through and carries on.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::invocation_log_delivery::logs;
use crate::kinesis_event_source_handler_writes_dynamodb::{dynamodb, given_table, KEY_ATTRIBUTE};
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for and every address names.
const REGION: &str = "us-east-1";

/// The stage both apis deploy, which is the one path segment a client's handshake and the
/// backend's callback URL carry.
const STAGE: &str = "prod";

/// The expression AWS documents for a WebSocket api whose messages name their route in an
/// `action` property, and the routes the apis below hold: the two lifecycle keys, the key one
/// registration frame's value matches, and the catch-all the other reaches.
const SELECTION_EXPRESSION: &str = "$request.body.action";
const CONNECT_ROUTE: &str = "$connect";
const DISCONNECT_ROUTE: &str = "$disconnect";
const DEFAULT_ROUTE: &str = "$default";
const REGISTER_ROUTE: &str = "register";

/// The two `action` values a client registers with: the one the api holds a route key for, and
/// one no key matches, which `$default` serves.
const REGISTER_ACTION: &str = "register";
const REGISTER_UI_ACTION: &str = "register_ui";

/// What the backend's own records call each kind of connection, and the kinds of frame it pushes.
const UI_ROLE: &str = "ui";
const AGENT_ROLE: &str = "agent";
const NODES_LIST: &str = "nodes_list";
const REGISTRATION_SUCCESS: &str = "registration_success";
const NODE_DISCOVERED: &str = "node_discovered";

/// The table the backend keeps its record of connections in, and the execution role every call it
/// makes is performed under.
const TABLE: &str = "websocket-registration-connections";
const ROLE: &str = "websocket-registration-role";

/// The two functions: the one that composes its callback address from the event it was given, and
/// the one that is configured with no way to reach `@connections` at all.
const PUSHING_FUNCTION: &str = "websocket-registration-registrar";
const SILENT_FUNCTION: &str = "websocket-registration-silent-registrar";

/// The environment variable the pushing function carries and the silent one does not: whether the
/// handler composes a callback address out of its event's own `requestContext`.
const CALLBACK_FROM_EVENT: &str = "CALLBACK_FROM_EVENT";

/// The one permission AWS publishes for the `@connections` surface, which is what admits the
/// pushes the backend makes.
const MANAGE_CONNECTIONS: &str = "ManageConnections";

/// The module and attribute the deployment package carries its handler as, which is the entry
/// [`package`] writes the source to.
const HANDLER: &str = "lambda_function.lambda_handler";

/// Where `CreateFunction` is bound, and the `State` an invocation needs.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const ACTIVE: &str = "Active";

/// The `Timeout` each function is configured with, in seconds: a run's own wall clock, which on a
/// loaded host has to cover starting its runtime as well as the whole fan-out. No case here is
/// about that limit, so it is given generous room.
const FUNCTION_TIMEOUT: u64 = 300;

/// The status RFC 6455 §4.2.2 requires of a COMPLETED opening handshake.
const SWITCHING_PROTOCOLS: u16 = 101;

/// What a `@connections` call is answered with for a connection the endpoint holds, and for one
/// that has gone: the status, the shape a `restJson1` client reads an error's shape from, and the
/// header it reads it in — which is where a MEMBERLESS error names itself.
const SERVED: u16 = 200;
const GONE: u16 = 410;
const GONE_CODE: &str = "GoneException";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The prefix of the group a function's runs are recorded in: "a default log group named
/// `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The payload this test itself posts to a connection, to read back as that connection's next
/// frame. Its content is this suite's own marker and nothing the backend ever sends.
const PROBE_PAYLOAD: &str = "{\"type\":\"probe\"}";

/// How long a leg that is OWED is waited for before this suite calls it pending: a handshake's
/// answer, a frame the backend pushed, a row it wrote, a record of a run, and the refusal a closed
/// connection's id earns. Every wait is for the CONDITION — bytes on the socket, an item in the
/// table, a status from the endpoint — and nothing here asserts on elapsed time; the budget is
/// well above any span these services choose while still reporting a leg that answers nothing.
const LEG_DEADLINE: Duration = Duration::from_secs(120);

/// How often a condition another service owes is looked at again.
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The backend every route of both apis fronts, written the way a WebSocket application is: it
/// keeps its own table of the connections it has admitted, answers each registration through the
/// `@connections` endpoint it composes from the event's OWN `requestContext` — the callback URL
/// AWS documents, `https://{domainName}/{stage}` — announces every new agent to the connections
/// it has a record of, and drops a record only for a connection its own push was told is GONE.
///
/// `$disconnect` deliberately leaves the row where it is: that is what makes a later fan-out meet
/// a connection whose socket has ended, which is the leg the refusal class decides.
fn handler_source() -> String {
    format!(
        r#"
import json
import os

import boto3
import botocore.exceptions


def rows_of(tables, table, api):
    answered = tables.scan(TableName=table)
    rows = []
    for item in answered.get("Items") or []:
        if item.get("apiId", {{}}).get("S", "") != api:
            continue
        rows.append({{
            "id": item["{key}"]["S"],
            "role": item.get("role", {{}}).get("S", ""),
            "node": item.get("node", {{}}).get("S", ""),
        }})
    rows.sort(key=lambda row: row["id"])
    return rows


def register(tables, table, connection, api, role, node):
    tables.put_item(
        TableName=table,
        Item={{
            "{key}": {{"S": connection}},
            "apiId": {{"S": api}},
            "role": {{"S": role}},
            "node": {{"S": node}},
        }},
    )


def push(tables, table, endpoint, target, payload):
    record = {{
        "target": target,
        "status": 0,
        "code": "",
        "delivered": False,
        "pruned": False,
    }}
    if not endpoint:
        print("nothing to answer %s through" % target)
        return record
    connections = boto3.client("apigatewaymanagementapi", endpoint_url=endpoint)
    try:
        answered = connections.post_to_connection(
            ConnectionId=target, Data=json.dumps(payload).encode("utf-8")
        )
        record["status"] = answered["ResponseMetadata"]["HTTPStatusCode"]
        record["delivered"] = True
    except botocore.exceptions.ClientError as error:
        record["status"] = error.response["ResponseMetadata"]["HTTPStatusCode"]
        record["code"] = error.response["Error"]["Code"]
        if record["code"] == "{gone}":
            tables.delete_item(TableName=table, Key={{"{key}": {{"S": target}}}})
            record["pruned"] = True
    return record


def lambda_handler(event, context):
    called = event["requestContext"]
    route = called["routeKey"]
    connection = called["connectionId"]
    api = called["apiId"]
    table = os.environ["TABLE"]
    tables = boto3.client("dynamodb")
    endpoint = ""
    if os.environ.get("{from_event}"):
        endpoint = "https://%s/%s" % (called["domainName"], called["stage"])
    if route == "{connect}":
        print("connect %s" % connection)
        return {{"statusCode": 200}}
    if route == "{disconnect}":
        print("disconnect %s" % connection)
        return {{"statusCode": 200}}
    try:
        message = json.loads(event.get("body") or "")
    except ValueError:
        message = {{}}
    if not isinstance(message, dict):
        message = {{}}
    action = message.get("action") or ""
    known = rows_of(tables, table, api)
    if action == "{register_ui}":
        register(tables, table, connection, api, "{ui}", "")
        nodes = sorted([row["node"] for row in known if row["role"] == "{agent}"])
        push(tables, table, endpoint, connection, {{
            "type": "{nodes_list}",
            "route": route,
            "connectionId": connection,
            "nodes": nodes,
        }})
        print("registered the ui %s on %s" % (connection, route))
        return {{"statusCode": 200}}
    if action == "{register}":
        node = message.get("node") or ""
        register(tables, table, connection, api, "{agent}", node)
        fanned = []
        for row in known:
            if row["role"] != "{ui}":
                continue
            fanned.append(push(tables, table, endpoint, row["id"], {{
                "type": "{node_discovered}",
                "route": route,
                "node": node,
                "connectionId": row["id"],
            }}))
        push(tables, table, endpoint, connection, {{
            "type": "{registration_success}",
            "route": route,
            "node": node,
            "connectionId": connection,
            "fanOut": fanned,
        }})
        delivered = len([one for one in fanned if one["delivered"]])
        pruned = len([one for one in fanned if one["pruned"]])
        print("fan-out of %s delivered %d pruned %d" % (node, delivered, pruned))
        return {{"statusCode": 200}}
    print("no registration in %s" % (event.get("body") or ""))
    return {{"statusCode": 200}}
"#,
        key = KEY_ATTRIBUTE,
        gone = GONE_CODE,
        from_event = CALLBACK_FROM_EVENT,
        connect = CONNECT_ROUTE,
        disconnect = DISCONNECT_ROUTE,
        register_ui = REGISTER_UI_ACTION,
        register = REGISTER_ACTION,
        ui = UI_ROLE,
        agent = AGENT_ROLE,
        nodes_list = NODES_LIST,
        node_discovered = NODE_DISCOVERED,
        registration_success = REGISTRATION_SUCCESS,
    )
}

/// One registration pass: the agent that registers, and the shape of the fan-out it must perform.
struct Row {
    case: &'static str,
    /// The value the registration frame carries, which every notification of that pass names.
    node: &'static str,
    /// Whether another ui client registers before this pass, so the fan-out addresses one more
    /// live connection than the pass before it.
    another_ui: bool,
    /// How many live connections the fan-out must DELIVER to, and how many records of departed
    /// connections it must PRUNE. Both are asserted against the live and stale sets the session
    /// has reached, so a row whose numbers drifted from the session fails before anything else.
    delivered: usize,
    pruned: usize,
}

/// The passes span the number of live connections one fan-out addresses, whether a departed
/// connection's record is among the ids it addresses, and the payload each registration carries.
const ROWS: &[Row] = &[
    Row {
        case: "an agent registering while one ui is live and one has departed",
        node: "node-alpha",
        another_ui: false,
        delivered: 1,
        pruned: 1,
    },
    Row {
        case: "the next agent registering, the departed record having been pruned",
        node: "n",
        another_ui: false,
        delivered: 1,
        pruned: 0,
    },
    Row {
        case: "an agent registering while two uis are live",
        node: "node-gamma-with-a-considerably-longer-name",
        another_ui: true,
        delivered: 2,
        pruned: 0,
    },
];

/// AWS Lambda, API Gateway V2, the API Gateway Management API, Amazon DynamoDB and Amazon
/// CloudWatch Logs over ONE shared engine, as `core/router/src/lib.rs` mounts them, with IAM and
/// the token service behind the execution role: the sockets the apis open, the functions their
/// routes front, the table those functions write and the groups their runs are recorded in are
/// all served here, and every SDK call a handler makes is dispatched back into this same router.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // What is under test is the session, not SigV4: each call carries a credential scope so the
    // router can name the target service, and a WebSocket client sends none at all.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One answered request: its status, the error shape it named and its body.
struct Answered {
    status: u16,
    /// The shape a `restJson1` client resolves a failure's error from — the `x-amzn-errortype`
    /// response header — which is the only place a MEMBERLESS error names itself.
    error_type: String,
    body: Value,
    text: String,
}

/// One JSON call of `service` at its regional host, as it was answered.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let payload = match &body {
        Some(value) => bytes::Bytes::from(value.to_string()),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization(service))
        .body(payload)
        .expect("the test request is well formed");
    answered_by(router, request).await
}

/// Dispatch one built request and read the answer the way every assertion below needs it.
async fn answered_by(
    router: &MultiServiceRouter,
    request: http::Request<bytes::Bytes>,
) -> Answered {
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        error_type: parts
            .headers
            .get(ERROR_TYPE_HEADER)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_default(),
        body: serde_json::from_slice(&answered).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&answered).to_string(),
    }
}

/// One AWS Lambda `restJson1` call.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answered {
    json_call(router, "lambda", method, uri, body).await
}

/// One API Gateway V2 control-plane call that has to be accepted with `expected`. The V2 endpoint
/// shares the control plane's `apigateway` signing name.
async fn control_plane(
    router: &MultiServiceRouter,
    operation: &str,
    uri: &str,
    body: Value,
    expected: u16,
) -> Value {
    let answered = json_call(router, "apigateway", "POST", uri, Some(body)).await;
    assert_eq!(
        answered.status, expected,
        "setup: {operation} must answer {expected}, got {} with {}",
        answered.status, answered.text
    );
    answered.body
}

/// A required string member of a JSON answer.
fn member(what: &str, answered: &Value, name: &str) -> String {
    answered
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{what} must report {name}, got {answered}"))
        .to_string()
}

/// Create the function `name` whose code is the backend's source and whose environment is
/// `variables`, and poll until AWS Lambda reports it `Active` — the state an invocation needs,
/// owed by the CLOCK, so nothing here sleeps on that service's behalf. Answers its ARN.
async fn given_function(
    router: &MultiServiceRouter,
    name: &str,
    role_arn: &str,
    variables: Value,
) -> String {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": variables },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&handler_source())),
            },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction {name} must answer 201, got {} with {}",
        created.status, created.text
    );
    let arn = member("CreateFunction", &created.body, "FunctionArn");
    let deadline = Instant::now() + LEG_DEADLINE;
    loop {
        let reported = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        if reported.body.get("State").and_then(Value::as_str) == Some(ACTIVE) {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup: AWS Lambda never reported {name} {ACTIVE}, last read {}",
            reported.text
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The resource-based grant that admits an invocation from API Gateway, without which the gateway
/// makes none.
async fn gateway_may_invoke(router: &MultiServiceRouter, name: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status, 201,
        "setup: AddPermission on {name} must answer 201, got {} with {}",
        granted.status, granted.text
    );
}

/// A WEBSOCKET api whose every route fronts `function_arn`, with a deployment released to the
/// stage a client addresses. Answers `(ApiId, the authority its ApiEndpoint announces)`.
async fn given_api(router: &MultiServiceRouter, name: &str, function_arn: &str) -> (String, String) {
    let created = control_plane(
        router,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": SELECTION_EXPRESSION,
        }),
        201,
    )
    .await;
    let api_id = member("CreateApi", &created, "apiId");
    let announced = member("CreateApi", &created, "apiEndpoint");
    let integration = control_plane(
        router,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_arn,
            "integrationMethod": "POST",
        }),
        201,
    )
    .await;
    let integration_id = member("CreateIntegration", &integration, "integrationId");
    for route_key in [
        CONNECT_ROUTE,
        DISCONNECT_ROUTE,
        REGISTER_ROUTE,
        DEFAULT_ROUTE,
    ] {
        control_plane(
            router,
            "CreateRoute",
            &format!("/v2/apis/{api_id}/routes"),
            json!({
                "routeKey": route_key,
                "target": format!("integrations/{integration_id}"),
            }),
            201,
        )
        .await;
    }
    control_plane(
        router,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": STAGE }),
        201,
    )
    .await;
    control_plane(
        router,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
        201,
    )
    .await;
    (api_id, announced_authority(&announced))
}

/// The authority a WEBSOCKET api's announced `ApiEndpoint` names, having checked the address is
/// the loopback one — so every dial below reaches this machine's own listener and nothing else.
/// Nothing about the address is spelled out here beyond that check: the scheme, the host and the
/// port all come from the value the control plane reported.
fn announced_authority(announced: &str) -> String {
    let authority = announced.strip_prefix("wss://").unwrap_or_else(|| {
        panic!("setup: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
    });
    let (host, _port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("setup: the announced socket names a host:port, got {announced:?}")
    });
    assert_eq!(
        host, "127.0.0.1",
        "setup: a case dials the emulator's own loopback listener and nothing else, got \
         {announced:?}"
    );
    authority.to_string()
}

/// The socket a client gets from a completed handshake, framed by the maintained RFC 6455
/// implementation — the same client seam this crate's other socket suites drive, so nothing here
/// states the protocol itself.
type ClientSocket = tokio_tungstenite::WebSocketStream<TcpStream>;

/// Dial the api's own announced socket and complete the opening handshake for [`STAGE`].
///
/// The client implementation generates the `Sec-WebSocket-Key` and VERIFIES the
/// `Sec-WebSocket-Accept` the server derived from it before it hands the socket back, so a server
/// answering a value it did not derive fails here rather than passing an assertion.
async fn upgraded(case: &str, authority: &str) -> ClientSocket {
    let port: u16 = authority
        .rsplit_once(':')
        .and_then(|(_host, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a port in {authority:?}"));
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the announced socket must accept a connection, got {error}")
        });
    let url = format!("ws://{authority}/{STAGE}");
    match tokio_tungstenite::client_async(&url, stream).await {
        Ok((socket, answered)) => {
            assert_eq!(
                answered.status().as_u16(),
                SWITCHING_PROTOCOLS,
                "case {case:?}: expected the handshake to be answered {SWITCHING_PROTOCOLS}, got \
                 {:?}",
                answered.status()
            );
            socket
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(answered)) => panic!(
            "case {case:?}: the deployed stage refused the handshake with {:?}",
            answered.status()
        ),
        Err(error) => panic!(
            "case {case:?}: the opening handshake reached no answer at all ({error}) — a client \
             that upgraded nothing and was refused nothing cannot act on either"
        ),
    }
}

/// Send one text frame on the connection.
async fn sent(case: &str, socket: &mut ClientSocket, message: &str) {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the client's frame must be sent, got {error}")
        });
}

/// The next DATA frame the api sends on `socket`, within [`LEG_DEADLINE`]. A deadline that passes
/// is what this suite exists for: a leg that answers nothing is the defect, so it fails here with
/// its case named instead of hanging the run.
async fn next_text(case: &str, what: &str, socket: &mut ClientSocket) -> String {
    let read = tokio::time::timeout(LEG_DEADLINE, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => return Some(Ok(text.as_str().to_string())),
                Some(Ok(Message::Binary(bytes))) => {
                    return Some(Ok(String::from_utf8_lossy(&bytes).to_string()))
                }
                // The protocol's own frames are not messages of the api: a ping the
                // implementation answers itself, and a pong, carry nothing of the backend's.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                Some(Ok(Message::Close(frame))) => return Some(Err(format!("{frame:?}"))),
                Some(Err(error)) => return Some(Err(error.to_string())),
                None => return None,
            }
        }
    })
    .await;
    match read {
        Ok(Some(Ok(text))) => text,
        Ok(Some(Err(ended))) => {
            panic!("case {case:?}: the socket ended instead of carrying {what}: {ended}")
        }
        Ok(None) => panic!("case {case:?}: the socket closed with {what} never pushed"),
        Err(_elapsed) => panic!(
            "case {case:?}: {what} never arrived on the open connection — every leg of the \
             session must answer or be refused, never leave the client waiting"
        ),
    }
}

/// Send one frame and read the reply the backend pushes back for it, as a JSON document.
async fn exchanged(case: &str, what: &str, socket: &mut ClientSocket, message: &str) -> Value {
    sent(case, socket, message).await;
    pushed_document(case, what, socket).await
}

/// The next document pushed to an already-open connection, with no frame sent first.
async fn pushed_document(case: &str, what: &str, socket: &mut ClientSocket) -> Value {
    let read = next_text(case, what, socket).await;
    serde_json::from_str(&read).unwrap_or_else(|error| {
        panic!("case {case:?}: {what} must be the backend's own document, got {read:?} ({error})")
    })
}

/// End the connection the way a client does: the closing handshake RFC 6455 §5.5.1 states, driven
/// by the same implementation, read to the end so the reply the server owes is read rather than
/// the connection being dropped under it.
async fn closed(case: &str, mut socket: ClientSocket) {
    socket.close(None).await.unwrap_or_else(|error| {
        panic!("case {case:?}: the client's close must be sent, got {error}")
    });
    let drained = tokio::time::timeout(LEG_DEADLINE, async {
        while let Some(Ok(_frame)) = socket.next().await {}
    })
    .await;
    assert!(
        drained.is_ok(),
        "case {case:?}: the closing handshake must complete, and the socket end with it"
    );
}

/// The frame a ui client registers with: an `action` value no route key matches, so `$default`
/// serves it.
fn register_ui_frame() -> String {
    json!({ "action": REGISTER_UI_ACTION }).to_string()
}

/// The frame an agent registers with: an `action` value the api holds a route key for.
fn register_frame(node: &str) -> String {
    json!({ "action": REGISTER_ACTION, "node": node }).to_string()
}

/// `value` as ONE path segment of a URI: a minted `ConnectionId` is standard base64, whose
/// alphabet carries `+`, `/` and `=`, and a client addressing that id sends it as a single
/// segment — escaped by the maintained RFC 3986 encoder rather than by a second spelling of the
/// grammar here.
fn percent_encoded(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// `PostToConnection` on `connection_id` at `api_id`'s published `@connections` endpoint, as it
/// was answered.
async fn post_to_connection(
    router: &MultiServiceRouter,
    api_id: &str,
    connection_id: &str,
    payload: &str,
) -> Answered {
    let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://{host}/{STAGE}/@connections/{encoded}",
            encoded = percent_encoded(connection_id)
        ))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("execute-api"))
        .body(bytes::Bytes::from(payload.to_string()))
        .expect("the test request is well formed");
    answered_by(router, request).await
}

/// Offer `PostToConnection` for `connection_id` until the endpoint refuses it as GONE, which is
/// owed by the close the client performed — the `$disconnect` leg the clock makes due — and
/// answer that refusal.
async fn refused_until_gone(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    connection_id: &str,
) -> Answered {
    let deadline = Instant::now() + LEG_DEADLINE;
    loop {
        let answered = post_to_connection(router, api_id, connection_id, PROBE_PAYLOAD).await;
        if answered.status == GONE {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the id of a connection that has ended to be refused {GONE}, \
             last answered {} with {}",
            answered.status,
            answered.text
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every item the backend's table holds, keyed by the connection id it is held under.
async fn held_rows(router: &MultiServiceRouter, case: &str) -> BTreeMap<String, Value> {
    let answered = dynamodb(router, case, "Scan", &json!({ "TableName": TABLE })).await;
    answered
        .get("Items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get(KEY_ATTRIBUTE)?.get("S")?.as_str()?.to_string();
                    Some((id, item.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The string value an item holds for `name`.
fn attribute(item: &Value, name: &str) -> String {
    item.get(name)
        .and_then(|value| value.get("S"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The ids the table holds for `api_id` in `role`, as a set.
fn rows_of(held: &BTreeMap<String, Value>, api_id: &str, role: &str) -> BTreeSet<String> {
    held.iter()
        .filter(|(_, item)| attribute(item, "apiId") == api_id && attribute(item, "role") == role)
        .map(|(id, _)| id.clone())
        .collect()
}

/// Poll until the table holds exactly one ui record of `api_id`, and answer the connection id it
/// is held under — the write a registration makes, read back through the service that owns it.
async fn registered_ui_of(router: &MultiServiceRouter, case: &str, api_id: &str) -> String {
    let deadline = Instant::now() + LEG_DEADLINE;
    loop {
        let held = held_rows(router, case).await;
        let found = rows_of(&held, api_id, UI_ROLE);
        if found.len() == 1 {
            return found.into_iter().next().unwrap_or_default();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the registration to leave exactly one {UI_ROLE} record of \
             {api_id:?} in {TABLE:?}, got {found:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Wait until `function`'s own log group holds a record containing `marker`, which is what an
/// operator debugging the session reads. A group nothing has written to yet does not exist, which
/// CloudWatch Logs answers as a refusal rather than as an empty list, so such an answer reads
/// here as no records.
async fn logged(router: &MultiServiceRouter, case: &str, function: &str, marker: &str) {
    let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
    let deadline = Instant::now() + LEG_DEADLINE;
    loop {
        let answered = logs(
            router,
            case,
            "FilterLogEvents",
            &json!({ "logGroupName": group }),
        )
        .await;
        let held: Vec<String> = answered
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event.get("message").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if held.iter().any(|record| record.contains(marker)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a record of {function} containing {marker:?}, got {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Assert one pushed document is of `kind`, served by `route`, and answer it for the members only
/// its own kind carries.
fn assert_kind(case: &str, what: &str, pushed: &Value, kind: &str, route: &str) {
    assert_eq!(
        (
            pushed.get("type").and_then(Value::as_str),
            pushed.get("route").and_then(Value::as_str)
        ),
        (Some(kind), Some(route)),
        "case {case:?}: expected {what} to be the {kind} the {route} route's integration pushed, \
         got {pushed}"
    );
}

/// The record a fan-out made for `target`, out of the summary the backend pushed to the agent.
fn fan_out_record<'a>(case: &str, summary: &'a Value, target: &str) -> &'a Value {
    summary
        .get("fanOut")
        .and_then(Value::as_array)
        .and_then(|records| {
            records
                .iter()
                .find(|record| record.get("target").and_then(Value::as_str) == Some(target))
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: the fan-out must name what it was answered for {target:?}, got {summary}")
        })
}

/// Assert what the backend's own SDK was answered for one id of the fan-out.
fn assert_answered(
    case: &str,
    record: &Value,
    status: u16,
    code: &str,
    delivered: bool,
    pruned: bool,
) {
    assert_eq!(
        (
            record.get("status").and_then(Value::as_u64),
            record.get("code").and_then(Value::as_str),
            record.get("delivered").and_then(Value::as_bool),
            record.get("pruned").and_then(Value::as_bool),
        ),
        (
            Some(u64::from(status)),
            Some(code),
            Some(delivered),
            Some(pruned),
        ),
        "case {case:?}: expected the push to be answered {status} {code:?}, delivered \
         {delivered} and the record pruned {pruned}, got {record}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registration_fan_out_reaches_every_live_connection_and_prunes_the_departed() {
    let router = emulator();

    // 1. The backend's own record of connections, the role its every call is performed under, and
    //    the two functions: the one that composes a callback address from its event, and the one
    //    that has none at all.
    given_table(&router, "the backend's own record", TABLE).await;
    let role_arn = given_role(
        &router,
        "the backend's execution role",
        ROLE,
        Some(json!([
            logs_only(),
            allow("dynamodb", &["PutItem", "DeleteItem", "Scan"], "*"),
            allow("execute-api", &[MANAGE_CONNECTIONS], "*"),
        ])),
    )
    .await;
    let mut pushing_variables = serde_json::Map::new();
    pushing_variables.insert("TABLE".to_string(), json!(TABLE));
    pushing_variables.insert(CALLBACK_FROM_EVENT.to_string(), json!("yes"));
    let pushing = given_function(
        &router,
        PUSHING_FUNCTION,
        &role_arn,
        Value::Object(pushing_variables),
    )
    .await;
    gateway_may_invoke(&router, PUSHING_FUNCTION).await;
    let silent = given_function(
        &router,
        SILENT_FUNCTION,
        &role_arn,
        json!({ "TABLE": TABLE }),
    )
    .await;
    gateway_may_invoke(&router, SILENT_FUNCTION).await;

    // 2. The two apis, each deployed to the stage its clients address.
    let (api, authority) = given_api(&router, "registration-fan-out", &pushing).await;
    let (quiet_api, quiet_authority) =
        given_api(&router, "registration-without-a-callback", &silent).await;

    // 3. The ui the fan-out must keep: it registers with a frame and is answered through
    //    `@connections`, on the connection that frame arrived on.
    let live_case = "a ui connection registering on a deployed stage";
    let mut live_ui = upgraded(live_case, &authority).await;
    let live_reply = exchanged(
        live_case,
        "the reply to a ui registration",
        &mut live_ui,
        &register_ui_frame(),
    )
    .await;
    assert_kind(
        live_case,
        "the reply to a ui registration",
        &live_reply,
        NODES_LIST,
        DEFAULT_ROUTE,
    );
    assert_eq!(
        live_reply.get("nodes"),
        Some(&json!([])),
        "case {live_case:?}: the first ui is told of the agents the backend holds, which is none \
         yet, got {live_reply}"
    );
    let live_id = member("the reply to a ui registration", &live_reply, "connectionId");

    // 4. The ui that DEPARTS: registered the same way, then closed, so the backend's record of it
    //    outlives its socket — which is what a later fan-out has to decide about.
    let departing_case = "a ui connection that registers and then departs";
    let mut departing_ui = upgraded(departing_case, &authority).await;
    let departing_reply = exchanged(
        departing_case,
        "the reply to the departing ui's registration",
        &mut departing_ui,
        &register_ui_frame(),
    )
    .await;
    let departed_id = member(
        "the reply to the departing ui's registration",
        &departing_reply,
        "connectionId",
    );
    assert_ne!(
        departed_id, live_id,
        "case {departing_case:?}: each connection of the stage is minted its own id, got \
         {departed_id:?} for both"
    );
    closed(departing_case, departing_ui).await;

    // Its id now addresses nothing: the memberless 410 a sender prunes its own record on, and the
    // only answer that tells it to.
    let refused = refused_until_gone(&router, departing_case, &api, &departed_id).await;
    assert!(
        refused.error_type.contains(GONE_CODE),
        "case {departing_case:?}: expected the refusal to name {GONE_CODE} in \
         {ERROR_TYPE_HEADER}, got {:?} with {:?}",
        refused.error_type,
        refused.text
    );
    assert_eq!(
        serde_json::from_str::<Value>(&refused.text).ok(),
        Some(json!({})),
        "case {departing_case:?}: a memberless error shape carries no body member, got {}",
        refused.text
    );

    // 5. Each registration pass: the agent's own reply, the notification every LIVE connection
    //    reads, what the backend's SDK was answered for each id it addressed, and the records it
    //    kept or pruned.
    let mut live: Vec<(String, ClientSocket)> = vec![(live_id.clone(), live_ui)];
    let mut stale: Vec<String> = vec![departed_id.clone()];
    let mut agents: Vec<(String, String)> = Vec::new();
    let mut agent_sockets: Vec<ClientSocket> = Vec::new();
    for row in ROWS {
        let case = row.case;
        if row.another_ui {
            let mut another = upgraded(case, &authority).await;
            let reply = exchanged(
                case,
                "the reply to another ui's registration",
                &mut another,
                &register_ui_frame(),
            )
            .await;
            assert_kind(
                case,
                "the reply to another ui's registration",
                &reply,
                NODES_LIST,
                DEFAULT_ROUTE,
            );
            let known: Vec<String> = agents.iter().map(|(node, _)| node.clone()).collect();
            let mut expected = known.clone();
            expected.sort();
            assert_eq!(
                reply.get("nodes").and_then(Value::as_array).map(|nodes| nodes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()),
                Some(expected.clone()),
                "case {case:?}: a ui registering is told of the agents registered before it \
                 ({expected:?}), got {reply}"
            );
            let id = member("the reply to another ui's registration", &reply, "connectionId");
            live.push((id, another));
        }
        assert_eq!(
            (row.delivered, row.pruned),
            (live.len(), stale.len()),
            "case {case:?}: this pass is declared to deliver to {} and prune {}, while the \
             session holds {} live and {} departed connections",
            row.delivered,
            row.pruned,
            live.len(),
            stale.len()
        );

        // The agent registers: its own reply carries what every push of the pass was answered.
        let mut agent = upgraded(case, &authority).await;
        let summary = exchanged(
            case,
            "the reply to an agent's registration",
            &mut agent,
            &register_frame(row.node),
        )
        .await;
        assert_kind(
            case,
            "the reply to an agent's registration",
            &summary,
            REGISTRATION_SUCCESS,
            REGISTER_ROUTE,
        );
        assert_eq!(
            summary.get("node").and_then(Value::as_str),
            Some(row.node),
            "case {case:?}: the reply names the registration it answers, got {summary}"
        );
        let agent_id = member("the reply to an agent's registration", &summary, "connectionId");

        // Every id the fan-out addressed, and the one answer each earned.
        let addressed: BTreeSet<String> = summary
            .get("fanOut")
            .and_then(Value::as_array)
            .map(|records| {
                records
                    .iter()
                    .filter_map(|record| record.get("target").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let owed: BTreeSet<String> = live
            .iter()
            .map(|(id, _)| id.clone())
            .chain(stale.iter().cloned())
            .collect();
        assert_eq!(
            addressed, owed,
            "case {case:?}: the fan-out addresses every connection the backend has a record of, \
             got {addressed:?} for {owed:?}"
        );
        for (id, _) in &live {
            let record = fan_out_record(case, &summary, id);
            assert_answered(case, record, SERVED, "", true, false);
        }
        for id in &stale {
            let record = fan_out_record(case, &summary, id);
            assert_answered(case, record, GONE, GONE_CODE, false, true);
        }

        // Every live connection reads the notification of this pass, on its own socket.
        for (id, socket) in &mut live {
            let notified =
                pushed_document(case, "the notification a live connection reads", socket).await;
            assert_kind(
                case,
                "the notification a live connection reads",
                &notified,
                NODE_DISCOVERED,
                REGISTER_ROUTE,
            );
            assert_eq!(
                (
                    notified.get("node").and_then(Value::as_str),
                    notified.get("connectionId").and_then(Value::as_str)
                ),
                (Some(row.node), Some(id.as_str())),
                "case {case:?}: each live connection is notified of {:?} on its own connection, \
                 got {notified}",
                row.node
            );
        }

        // And the records that leaves: the live uis and every agent so far are held, and the
        // departed connection's record is gone — pruned by the refusal and by nothing else.
        agents.push((row.node.to_string(), agent_id.clone()));
        let held = held_rows(&router, case).await;
        let held_uis = rows_of(&held, &api, UI_ROLE);
        let expected_uis: BTreeSet<String> = live.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            held_uis, expected_uis,
            "case {case:?}: the backend keeps a record of exactly the live uis, got {held_uis:?} \
             for {expected_uis:?}"
        );
        let held_agents = rows_of(&held, &api, AGENT_ROLE);
        let expected_agents: BTreeSet<String> =
            agents.iter().map(|(_, id)| id.clone()).collect();
        assert_eq!(
            held_agents, expected_agents,
            "case {case:?}: every agent that registered is held, got {held_agents:?} for \
             {expected_agents:?}"
        );
        for id in &stale {
            assert!(
                !held.contains_key(id),
                "case {case:?}: the record of {id:?} must be pruned once its push was refused \
                 {GONE}, got {held:?}"
            );
        }
        stale.clear();

        // The run's own record of the whole pass.
        logged(
            &router,
            case,
            PUSHING_FUNCTION,
            &format!(
                "fan-out of {} delivered {} pruned {}",
                row.node, row.delivered, row.pruned
            ),
        )
        .await;
        // The agent's socket is held for the rest of the session: a dropped socket ends its
        // connection, which would have the api perform a `$disconnect` nobody asked for while
        // the passes after it run.
        agent_sockets.push(agent);
    }

    // 6. The pass whose backend has NO callback address: its write succeeds, and the connection is
    //    delivered nothing. The next frame that connection reads is the one this test POSTS to it,
    //    which a reply pushed earlier would have come before — frames of one connection are
    //    ordered — so a fabricated answer would be read here instead.
    let quiet_case = "a registration whose backend composes no callback address";
    let mut quiet_ui = upgraded(quiet_case, &quiet_authority).await;
    sent(quiet_case, &mut quiet_ui, &register_ui_frame()).await;
    let quiet_id = registered_ui_of(&router, quiet_case, &quiet_api).await;
    logged(
        &router,
        quiet_case,
        SILENT_FUNCTION,
        &format!("nothing to answer {quiet_id} through"),
    )
    .await;
    let probe = post_to_connection(&router, &quiet_api, &quiet_id, PROBE_PAYLOAD).await;
    assert_eq!(
        probe.status, SERVED,
        "case {quiet_case:?}: the connection a registration admitted is live, so a push to it is \
         served {SERVED}, got {} with {}",
        probe.status, probe.text
    );
    let read = next_text(quiet_case, "the first frame of that connection", &mut quiet_ui).await;
    assert_eq!(
        read, PROBE_PAYLOAD,
        "case {quiet_case:?}: the frames a connection reads are the ones something posted to it, \
         so the first is this test's own probe, got {read:?}"
    );

    // Nothing of the session depends on the agents' sockets any more: they are closed here, the
    // way a client ends one, rather than dropped under the api.
    for (at, socket) in agent_sockets.into_iter().enumerate() {
        closed(&format!("the agent socket {at} of the session"), socket).await;
    }
}
