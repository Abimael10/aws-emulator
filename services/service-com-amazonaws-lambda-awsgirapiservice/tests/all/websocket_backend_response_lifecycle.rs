// SPDX-License-Identifier: CC0-1.0

//! The WHOLE backend-response lifecycle of a function fronting the routes of a deployed
//! WEBSOCKET api, driven over a real socket: the opening handshake, the `$connect` integration,
//! a client frame, the reply this backend pushes back through `@connections`, the state it
//! wrote, the records of its runs, and the close.
//!
//! > **RULE: every leg of a WEBSOCKET api's message round trip reaches a DEFINITE outcome, and
//! > each is reached from the leg before it. A correct opening handshake against a deployed
//! > stage is answered `101 Switching Protocols` with the accept value derived from the
//! > client's own key; over the established connection each text frame is dispatched to the
//! > route the api's `RouteSelectionExpression` selects — `$default` where the expression
//! > reaches no value or a value no route key matches — and that route's integration RUNS, with
//! > `requestContext` naming the connection, the stage and the host the client addressed, so the
//! > callback URL the handler composes from `domainName` and `stage` is an address its own
//! > `@connections` client is served at: the payload it posts there is delivered to that
//! > connection as a frame the client reads back. What the integration wrote is held by the
//! > services that own it, and what its run printed is in the function's log group. Once the
//! > client closes, the `$disconnect` route's integration is performed and the connection's id
//! > addresses nothing: a `@connections` call naming it is refused `410 GoneException`. No leg
//! > is ever left pending — a client that sent a frame is answered or refused, never kept
//! > waiting.**
//!
//! WHY the whole chain and not one leg: a client of a WebSocket backend learns nothing from a
//! handshake that upgrades if the frame it then sends reaches no route, and nothing from a route
//! that runs if the reply its backend pushes never arrives. A leg that answers neither a frame
//! nor a refusal cannot be told from a hung network, so retry and error handling have nothing to
//! act on and the application simply stops — which is what every bounded wait below fails on.
//!
//! The backend is this crate's: AWS Lambda runs the function behind every route, its in-process
//! SDK seam carries the `@connections` push the handler makes, and it delivers the run's records
//! to CloudWatch Logs. The services it is driven against are mounted beside it on ONE engine —
//! API Gateway V2 serves the socket, selects the route and performs the integration, the API
//! Gateway Management API answers the push, Amazon DynamoDB and Amazon S3 hold what the handler
//! wrote, Amazon CloudWatch Logs holds the records, and IAM and the token service answer for
//! what the execution role those calls are made under may do.
//!
//! The rows differ along what the rule generalises over: the frame's own form (a JSON object
//! naming the route it selects, the same route with another payload, a selector value no route
//! key matches, an object carrying no selector member at all, and text that is not JSON), and
//! so the route that serves it (a named route and `$default`) and the payload the backend reads
//! out of it. Every row is driven over the SAME established connection, so a leg that only works
//! once is caught too.
//!
//! The client half of the conversation — the opening handshake, the masking of every frame it
//! sends, the accept value it verifies and the closing handshake — is the maintained RFC 6455
//! implementation (`tokio-tungstenite`), the same one the API Gateway V2 crate's own socket
//! suites drive, so the protocol is stated nowhere in this file.
//!
//! What the neighbouring suites own and this one does not restate: the statuses the handshake
//! itself earns for each state of the api's records, and the frame-to-route selection over every
//! expression form, are the API Gateway V2 crate's `websocket_transport_data_plane.rs` and
//! `websocket_lambda_routes.rs`; the authorization of a push on the stage its client was built
//! with is `core/router/tests/all/lambda_handler_posts_to_its_websocket_connections.rs`.

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
use crate::s3_package_source::{s3_ok, s3_request};
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for and every address names.
const REGION: &str = "us-east-1";

/// The stage the api deploys, which is the one segment a client's path and the backend's
/// callback URL carry.
const STAGE: &str = "prod";

/// The expression AWS documents for a WebSocket api whose messages carry an `action` property,
/// and the route key one of those values selects. A value no key matches, and a frame the
/// expression reaches no value in, are served by the reserved `$default` route.
const SELECTION_EXPRESSION: &str = "$request.body.action";
const SEND_ROUTE: &str = "sendmessage";
const DEFAULT_ROUTE: &str = "$default";
const CONNECT_ROUTE: &str = "$connect";
const DISCONNECT_ROUTE: &str = "$disconnect";

/// The function this suite deploys, the execution role it runs as, and the table and bucket its
/// handler keeps its state in.
const FUNCTION: &str = "websocket-backend-response-lifecycle";
const ROLE: &str = "websocket-backend-response-lifecycle-role";
const TABLE: &str = "websocket-backend-response-connections";
const BUCKET: &str = "websocket-backend-response-archive";

/// The one permission AWS publishes for the `@connections` surface, which is what admits the
/// push the handler makes; the stores it writes beside it are its own table and bucket.
const MANAGE_CONNECTIONS: &str = "ManageConnections";

/// The module and attribute the deployment package carries its handler as, which is the entry
/// [`package`] writes the source to.
const HANDLER: &str = "lambda_function.lambda_handler";

/// Where `CreateFunction` is bound, and the `State` an invocation needs.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const ACTIVE: &str = "Active";

/// The `Timeout` the function is configured with, in seconds: a run's own wall clock, which on a
/// loaded host has to cover starting its runtime as well as the push it makes. No case here is
/// about that limit, so it is given generous room.
const FUNCTION_TIMEOUT: u64 = 300;

/// The status RFC 6455 §4.2.2 requires of a COMPLETED opening handshake.
const SWITCHING_PROTOCOLS: u16 = 101;

/// What a `@connections` call naming a connection that has ended is refused with: the status,
/// and the shape it names in the header a `restJson1` client reads an error's shape from — which
/// is where this one names itself, `GoneException` being memberless.
const GONE: u16 = 410;
const GONE_CODE: &str = "GoneException";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The prefix of the group a function's runs are recorded in: "a default log group named
/// `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// How long a leg that is OWED is waited for before this suite calls it pending: the handshake's
/// answer, the frame the backend pushes back, the record of a run and the refusal a closed
/// connection's id earns. Every one of them waits for the CONDITION — bytes on the socket, a
/// status from the endpoint, a record in the group — and nothing here asserts on elapsed time;
/// the budget is well above any span these services choose (one run of this handler, its runtime
/// started, takes about two seconds) while still reporting a leg that answers nothing promptly.
const LEG_DEADLINE: Duration = Duration::from_secs(60);

/// How often a condition another service owes is looked at again.
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The handler every route of the api fronts. It is one function for the whole lifecycle, the
/// way a WebSocket backend is written: it registers the connection it is admitting, and for each
/// message it archives what the frame carried, records that message under the key it archived it
/// at (naming the connection it arrived on), pushes the reply back through the `@connections`
/// endpoint it composes from the event's OWN `requestContext` — `https://{domainName}/{stage}`,
/// the callback URL AWS documents — and prints what it pushed.
fn handler_source() -> String {
    format!(
        r#"
import json
import os

import boto3


def lambda_handler(event, context):
    called = event["requestContext"]
    route = called["routeKey"]
    connection = called["connectionId"]
    stage = called["stage"]
    table = os.environ["TABLE"]
    tables = boto3.client("dynamodb")
    if route == "{CONNECT_ROUTE}":
        tables.put_item(
            TableName=table,
            Item={{
                "{KEY_ATTRIBUTE}": {{"S": connection}},
                "stage": {{"S": stage}},
                "state": {{"S": "connected"}},
            }},
        )
        print("connected %s on %s" % (connection, stage))
        return {{"statusCode": 200}}
    if route == "{DISCONNECT_ROUTE}":
        print("disconnected %s on %s" % (connection, stage))
        return {{"statusCode": 200}}
    body = event.get("body") or ""
    try:
        note = json.loads(body).get("note") or ""
    except (ValueError, AttributeError):
        note = body
    key = "notes/%s.json" % called["requestId"]
    boto3.client("s3").put_object(
        Bucket=os.environ["BUCKET"],
        Key=key,
        Body=json.dumps({{"route": route, "note": note}}).encode("utf-8"),
    )
    tables.put_item(
        TableName=table,
        Item={{
            "{KEY_ATTRIBUTE}": {{"S": key}},
            "connectionId": {{"S": connection}},
            "stage": {{"S": stage}},
            "state": {{"S": "served"}},
            "lastRoute": {{"S": route}},
            "lastNote": {{"S": note}},
        }},
    )
    endpoint = "https://%s/%s" % (called["domainName"], stage)
    answered = boto3.client(
        "apigatewaymanagementapi", endpoint_url=endpoint
    ).post_to_connection(
        ConnectionId=connection,
        Data=json.dumps(
            {{"route": route, "note": note, "connectionId": connection, "key": key}}
        ).encode("utf-8"),
    )
    print(
        "pushed %s on %s (%s)"
        % (note, route, answered["ResponseMetadata"]["HTTPStatusCode"])
    )
    return {{"statusCode": 200}}
"#
    )
}

/// One frame the client sends, and the outcome the whole round trip owes it.
struct Row {
    case: &'static str,
    /// The text frame sent on the connection.
    frame: &'static str,
    /// The route the api's `RouteSelectionExpression` selects for it.
    route: &'static str,
    /// What the backend reads out of that frame and pushes back.
    note: &'static str,
}

/// The rows span the frame's own form, and so the route that serves it and the payload the
/// backend reads: a selector value a route key matches (twice, with different payloads), a
/// selector value no key matches, an object the expression reaches no value in, and text that is
/// not JSON at all.
const ROWS: &[Row] = &[
    Row {
        case: "a frame whose selector value names a route of the api",
        frame: r#"{"action":"sendmessage","note":"the first note"}"#,
        route: SEND_ROUTE,
        note: "the first note",
    },
    Row {
        case: "another frame of the same route, carrying another payload",
        frame: r#"{"action":"sendmessage","note":"a second, longer note"}"#,
        route: SEND_ROUTE,
        note: "a second, longer note",
    },
    Row {
        case: "a frame whose selector value matches no route key",
        frame: r#"{"action":"no-such-route","note":"routed by default"}"#,
        route: DEFAULT_ROUTE,
        note: "routed by default",
    },
    Row {
        case: "a frame carrying no selector member at all",
        frame: r#"{"note":"no action member"}"#,
        route: DEFAULT_ROUTE,
        note: "no action member",
    },
    Row {
        case: "a frame that is not JSON at all",
        frame: "not json at all",
        route: DEFAULT_ROUTE,
        note: "not json at all",
    },
];

/// AWS Lambda, API Gateway V2, Amazon DynamoDB, Amazon S3 and Amazon CloudWatch Logs over ONE
/// shared engine, as `core/router/src/lib.rs` mounts them: the socket the api opens, the
/// function its routes front, the stores that function writes and the group its runs are
/// recorded in are all served here, and the handler's own SDK calls are dispatched back into
/// this same router.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // What is under test is the lifecycle, not SigV4: each call carries a credential scope so
    // the router can name the target service, and a WebSocket client sends none at all.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    // The `@connections` management API the backend's own push is a call of.
    router.register_service(
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    // The two stores the authorization gate reads: what the execution role may do is IAM's
    // answer, and the session its calls are signed with lives in the token service's store.
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
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    let text = String::from_utf8_lossy(&answered).to_string();
    Answered {
        status: parts.status.as_u16(),
        error_type: parts
            .headers
            .get(ERROR_TYPE_HEADER)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_default(),
        body: serde_json::from_slice(&answered).unwrap_or(Value::Null),
        text,
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

/// One API Gateway V2 control-plane call that has to be accepted with `expected`. The V2
/// endpoint shares the control plane's `apigateway` signing name.
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
        .unwrap_or_else(|| panic!("setup: {what} must report {name}, got {answered}"))
        .to_string()
}

/// Create the function whose code is `source` and poll until AWS Lambda reports it `Active` —
/// the state an invocation needs, owed by the CLOCK, so nothing here sleeps on that service's
/// behalf. Answers its ARN.
async fn given_function(router: &MultiServiceRouter, role_arn: &str, source: &str) -> String {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { "TABLE": TABLE, "BUCKET": BUCKET } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
            },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction must answer 201, got {} with {}",
        created.status, created.text
    );
    let arn = member("CreateFunction", &created.body, "FunctionArn");
    let deadline = Instant::now() + LEG_DEADLINE;
    loop {
        let reported = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if reported.body.get("State").and_then(Value::as_str) == Some(ACTIVE) {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup: AWS Lambda never reported {FUNCTION} {ACTIVE}, last read {}",
            reported.text
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The resource-based grant that admits an invocation from API Gateway, without which the
/// gateway makes none.
async fn gateway_may_invoke(router: &MultiServiceRouter) {
    let granted = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status, 201,
        "setup: AddPermission must answer 201, got {} with {}",
        granted.status, granted.text
    );
}

/// The WEBSOCKET api whose every route fronts that function, with a deployment released to the
/// stage a client addresses. Answers `(ApiId, the authority its ApiEndpoint announces)`.
async fn given_api(router: &MultiServiceRouter, function_arn: &str) -> (String, String) {
    let created = control_plane(
        router,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": "backend-response-lifecycle",
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
    for route_key in [CONNECT_ROUTE, DISCONNECT_ROUTE, SEND_ROUTE, DEFAULT_ROUTE] {
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
/// the loopback one — so the dial below reaches this machine's own listener and nothing else.
/// Nothing about the address is spelled out here beyond that check: the scheme, the host and the
/// port all come from the value the control plane reported.
fn announced_authority(announced: &str) -> String {
    let authority = announced.strip_prefix("wss://").unwrap_or_else(|| {
        panic!("setup: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
    });
    let (host, _port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("setup: the announced socket names a host:port, got {announced:?}"));
    assert_eq!(
        host, "127.0.0.1",
        "setup: a case dials the emulator's own loopback listener and nothing else, got \
         {announced:?}"
    );
    authority.to_string()
}

/// The socket a client gets from a completed handshake, framed by the maintained RFC 6455
/// implementation — the same client seam the API Gateway V2 crate's own socket suites drive, so
/// nothing here states the protocol itself.
type ClientSocket = tokio_tungstenite::WebSocketStream<TcpStream>;

/// Dial the api's own announced socket and complete the opening handshake for [`STAGE`].
///
/// The URL names the authority the api itself ANNOUNCED ([`announced_authority`], which holds
/// that address to the loopback literal), so the connection is made to the emulator's own
/// listener and to nothing else, and the host the client addressed — which is what the gateway
/// reports to the backend as `domainName`, and so the host of the callback URL that backend
/// composes — is the one the api published. The client implementation generates the
/// `Sec-WebSocket-Key` and VERIFIES the `Sec-WebSocket-Accept` the server derived from it before
/// it hands the socket back, so a server answering a value it did not derive fails here rather
/// than passing an assertion.
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

/// Send one text frame on the connection and read the next DATA frame the api sends back, within
/// [`LEG_DEADLINE`]. A deadline that passes is what this suite exists for: a leg that answers
/// nothing is the defect, so it fails here with its case named instead of hanging the run.
async fn exchanged(case: &str, socket: &mut ClientSocket, message: &str) -> String {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the client's frame must be sent, got {error}")
        });
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
            panic!("case {case:?}: the socket ended instead of carrying the reply: {ended}")
        }
        Ok(None) => panic!("case {case:?}: the socket closed with no reply pushed back"),
        Err(_elapsed) => panic!(
            "case {case:?}: no frame arrived on the open connection — the frame's route and the \
             push behind it must each answer or be refused, never leave the client waiting"
        ),
    }
}

/// End the connection the way a client does: the closing handshake RFC 6455 §5.5.1 states,
/// driven by the same implementation, read to the end so the reply the server owes is read
/// rather than the connection being dropped under it.
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

/// `PostToConnection` on `connection_id` at the api's published `@connections` endpoint, as it
/// was answered.
async fn post_to_connection(
    router: &MultiServiceRouter,
    api_id: &str,
    connection_id: &str,
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
        .body(bytes::Bytes::from_static(b"after the close"))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        error_type: parts
            .headers
            .get(ERROR_TYPE_HEADER)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_default(),
        body: Value::Null,
        text: String::from_utf8_lossy(&answered).to_string(),
    }
}

/// `value` as ONE path segment of a URI: a minted `ConnectionId` is standard base64, whose
/// alphabet carries `+`, `/` and `=`, and a client addressing that id sends it as a single
/// segment — escaped by the maintained RFC 3986 encoder this crate already binds a REST label's
/// value with, rather than by a second spelling of the grammar here.
fn percent_encoded(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// The item [`TABLE`] holds under `id`, read strongly consistently so that what is read is what
/// the handler wrote, polled until it is there in `state`.
async fn item_in_state(router: &MultiServiceRouter, case: &str, id: &str, state: &str) -> Value {
    let deadline = Instant::now() + LEG_DEADLINE;
    loop {
        let read = dynamodb(
            router,
            case,
            "GetItem",
            &json!({
                "TableName": TABLE,
                "Key": { KEY_ATTRIBUTE: { "S": id } },
                "ConsistentRead": true,
            }),
        )
        .await;
        let held = read.get("Item").cloned().unwrap_or(Value::Null);
        if string_member(&held, "state").as_deref() == Some(state) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the integration must leave {id:?} in {TABLE:?} with state {state:?}, \
             last read {held}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The string value an item holds for `name`.
fn string_member(item: &Value, name: &str) -> Option<String> {
    item.get(name)
        .and_then(|value| value.get("S"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Wait until the function's own log group holds a record containing `marker`, which is what an
/// operator debugging the lifecycle reads. A group nothing has written to yet does not exist,
/// which CloudWatch Logs answers as a refusal rather than as an empty list, so such an answer
/// reads here as no records.
async fn records_of(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let group = format!("{DEFAULT_GROUP_PREFIX}{FUNCTION}");
    let answered = logs(
        router,
        case,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn logged(router: &MultiServiceRouter, case: &str, marker: &str) {
    let deadline = Instant::now() + LEG_DEADLINE;
    loop {
        let held = records_of(router, case).await;
        if held.iter().any(|record| record.contains(marker)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the run's log to hold a record containing {marker:?}, got \
             {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_websocket_backend_completes_every_leg_of_its_response_lifecycle() {
    let router = emulator();
    let setup = "the backend's own stores";

    // 1. The state the backend keeps: the table it registers connections in and the bucket it
    //    archives each message to, both created through the services that own them.
    given_table(&router, setup, TABLE).await;
    s3_ok(&router, setup, "PUT", &format!("/{BUCKET}"), &[]).await;

    // 2. The execution role the whole lifecycle is performed under, the function every route of
    //    the api fronts, and the grant that admits the gateway's invocation of it.
    let role_arn = given_role(
        &router,
        setup,
        ROLE,
        Some(json!([
            logs_only(),
            allow("dynamodb", &["PutItem", "GetItem"], "*"),
            allow("s3", &["PutObject", "GetObject"], "*"),
            allow("execute-api", &[MANAGE_CONNECTIONS], "*"),
        ])),
    )
    .await;
    let function_arn = given_function(&router, &role_arn, &handler_source()).await;
    gateway_may_invoke(&router).await;

    // 3. The api: one AWS_PROXY integration on that function, the lifecycle and message routes
    //    bound to it, and a deployment released to the stage a client addresses.
    let (api_id, authority) = given_api(&router, &function_arn).await;

    // 4. The opening handshake, at the address the api announced: completed, with the accept
    //    value the client's own key derives verified by the client implementation itself.
    let upgrade = "the opening handshake of a deployed stage";
    let mut socket = upgraded(upgrade, &authority).await;

    // 5. Each row over that ONE connection: the frame, the reply the backend pushed back through
    //    its own `@connections` client, and what the integration left behind.
    let mut connection_id = String::new();
    for row in ROWS {
        let case = row.case;
        let reply = exchanged(case, &mut socket, row.frame).await;
        let pushed: Value = serde_json::from_str(&reply).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the pushed frame must be the backend's payload, got {reply:?} \
                 ({error})"
            )
        });
        if pushed.get("route").and_then(Value::as_str) != Some(row.route) {
            let records = records_of(&router, case).await;
            panic!(
                "case {case:?}: expected the frame to be dispatched to {:?}, got {pushed}; the \
                 run's log held {records:?}",
                row.route
            );
        }
        assert_eq!(
            pushed.get("note").and_then(Value::as_str),
            Some(row.note),
            "case {case:?}: expected the backend to read {:?} out of the frame, got {pushed}",
            row.note
        );
        let reported = pushed
            .get("connectionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !reported.is_empty(),
            "case {case:?}: the event must name the connection the frame arrived on, got {pushed}"
        );
        if connection_id.is_empty() {
            connection_id = reported.clone();
        }
        assert_eq!(
            reported, connection_id,
            "case {case:?}: every call of one connection reports the same id, got {pushed}"
        );

        // What the integration WROTE, read back through the services that own it: the archived
        // message, and the row naming the route that served this frame.
        let key = pushed
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let archived = s3_request(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
        assert_eq!(
            archived.status,
            200,
            "case {case:?}: the object the integration wrote must be readable at {key:?}, got {} \
             with {:?}",
            archived.status,
            String::from_utf8_lossy(&archived.body)
        );
        let held: Value = serde_json::from_slice(&archived.body).unwrap_or(Value::Null);
        assert_eq!(
            (
                held.get("route").and_then(Value::as_str),
                held.get("note").and_then(Value::as_str)
            ),
            (Some(row.route), Some(row.note)),
            "case {case:?}: expected the archived object to carry the route and the payload, got \
             {held}"
        );
        let item = item_in_state(&router, case, &key, "served").await;
        assert_eq!(
            (
                string_member(&item, "lastRoute").as_deref(),
                string_member(&item, "lastNote").as_deref(),
                string_member(&item, "stage").as_deref(),
                string_member(&item, "connectionId").as_deref()
            ),
            (
                Some(row.route),
                Some(row.note),
                Some(STAGE),
                Some(connection_id.as_str())
            ),
            "case {case:?}: expected the message's row to name the route, the payload, the stage \
             and the connection, got {item}"
        );

        // And the run's own record.
        logged(
            &router,
            case,
            &format!("pushed {} on {}", row.note, row.route),
        )
        .await;
    }

    // 6. The `$connect` leg, read now that the connection's own id is known: the row that
    //    integration wrote before the handshake was answered is still there, untouched by every
    //    message that followed, and the record of its run names the connection and the stage.
    let connected = item_in_state(&router, upgrade, &connection_id, "connected").await;
    assert_eq!(
        string_member(&connected, "stage").as_deref(),
        Some(STAGE),
        "case {upgrade:?}: expected the $connect integration's own row for {connection_id:?} to \
         name the stage that admitted it, got {connected}"
    );
    logged(
        &router,
        upgrade,
        &format!("connected {connection_id} on {STAGE}"),
    )
    .await;

    // 7. The close: the `$disconnect` route's integration is performed, and the id then
    //    addresses nothing — a definite refusal rather than a call left pending.
    let closing = "the connection the client closed";
    closed(closing, socket).await;
    logged(&router, closing, &format!("disconnected {connection_id}")).await;
    let deadline = Instant::now() + LEG_DEADLINE;
    let mut refused = post_to_connection(&router, &api_id, &connection_id).await;
    while refused.status != GONE && Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
        refused = post_to_connection(&router, &api_id, &connection_id).await;
    }
    assert_eq!(
        refused.status, GONE,
        "case {closing:?}: expected HTTP {GONE} for the id of a connection that has ended, got {} \
         with {:?}",
        refused.status, refused.text
    );
    assert!(
        refused.error_type.contains(GONE_CODE),
        "case {closing:?}: expected the refusal to name the {GONE_CODE} class in \
         {ERROR_TYPE_HEADER}, got {:?} with {:?}",
        refused.error_type,
        refused.text
    );
}
