// SPDX-License-Identifier: CC0-1.0

//! The WHOLE life of a WEBSOCKET api's connection, observed from OUTSIDE the emulator: the
//! connection request, the frames in between and the close, each served by the route that call
//! belongs to, with the lifecycle calls made visible to a client through the `@connections`
//! push their own handlers make.
//!
//! RULE: **every call of a connection of a deployed WEBSOCKET stage is served by performing
//! the integration of the route that call belongs to — the `$connect` route for the opening
//! handshake (`requestContext.eventType` `CONNECT`, with the handshake's query string), the
//! route the api's `RouteSelectionExpression` selects for each frame (`MESSAGE`), and the
//! `$disconnect` route for the close (`DISCONNECT`) — all three under the ONE
//! `requestContext.connectionId` that handshake minted, which no other connection of the stage
//! shares; a two-way route's integration answer is written back on the socket the frame came
//! from, and nothing else is, so a one-way lifecycle route — whose answer reaches no client at
//! all — is observable only through what it PUSHES, and a `PostToConnection` it addresses to
//! another live connection of the stage is delivered on that connection's socket.**
//!
//! `Route$RouteKey` gives `$connect`, `$disconnect` and `$default` as the reserved keys of a
//! WebSocket api, `Api$RouteSelectionExpression` is "the route selection expression for the
//! API" (`$request.body.action` for messages that name their route in an `action` member), and
//! "to define a two-way route, create a route response" (`CreateRouteResponse`).
//!
//! FIVE crates have to agree for that to hold — CloudFormation archives the inline handlers
//! into deployment packages this emulator can run, IAM holds the execution role those
//! declarations name, Lambda holds each route's function and RUNS it, API Gateway v2 serves the
//! socket and dispatches the three kinds of call, and the API Gateway Management API delivers
//! the push a lifecycle handler makes — so the suite lives here rather than in any one of them.
//!
//! Nothing here is a value this emulator fills in for the test: every assertion is on a record
//! a FUNCTION built and returned or pushed, naming itself by `context.function_name`, so a
//! lifecycle call that was never dispatched, or dispatched to the wrong route's function, is
//! read as a missing or mis-named record rather than as something that merely arrived.
//!
//! The rows span what the rule generalises over: what the connection sends (a frame naming a
//! route the api holds, a frame naming nothing — served by the catch-all — and no frame at
//! all), the query string the handshake carries, and the number of connections the stage holds
//! at once (a watcher plus one row's client, with every id distinct across the whole suite).
//!
//! What the owning crate's own suites pin and this one does not restate: the handshake's own
//! statuses for a stage or route the api's records serve nothing for, the frame refusals, and
//! the `@connections` refusal classes
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/websocket_transport_data_plane.rs`,
//! `.../websocket_message_refusals.rs`, `.../websocket_endpoint_connections.rs`). What is added
//! here is the LIFECYCLE as a client sees it: a $connect and a $disconnect invocation that
//! reach a client at all, over the address the api announces, on an emulator assembled from the
//! services that own each leg.
//!
//! The address dialled here is the one the api ANNOUNCES as its `ApiEndpoint`, which is where a
//! client that honours that value goes. A handshake written at the emulator's own service
//! endpoint instead is a separate gap, with its own open report
//! (`issues/things-that-do-not-work/20261104T160000Z-apigatewayv2-websocket-execute-api-upgrade-remains-unservable.md`):
//! nothing below covers that entry path.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::apigatewayv2_websocket_connect_registers_its_agent::{
    announced_loopback, read_head, ACCEPT_FIELD, RFC_EXAMPLE_ACCEPT, RFC_EXAMPLE_KEY,
};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request below is signed for, and the Region the api and its functions live
/// in.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, which is the path segment every handshake names.
const STAGE: &str = "prod";

/// The execution role the template names, created in IAM first so the declaration names a role
/// that service really holds.
const ROLE_NAME: &str = "apigwv2-ws-lifecycle-exec";

/// The stack that provisions the four functions, and the logical ids inside it.
const STACK: &str = "apigwv2-ws-lifecycle";

/// The four functions, one per route: the reserved lifecycle keys, the route a frame's `action`
/// names, and the catch-all. Each is its OWN function, so the record a call produces names which
/// one ran.
const CONNECT_FUNCTION: &str = "ws-lifecycle-connect";
const DISCONNECT_FUNCTION: &str = "ws-lifecycle-disconnect";
const PING_FUNCTION: &str = "ws-lifecycle-ping";
const DEFAULT_FUNCTION: &str = "ws-lifecycle-default";

/// The route keys those functions are fronted by.
const CONNECT_ROUTE: &str = "$connect";
const DISCONNECT_ROUTE: &str = "$disconnect";
const PING_ROUTE: &str = "ping";
const DEFAULT_ROUTE: &str = "$default";

/// The `eventType` each kind of call is reported under.
const CONNECT_EVENT: &str = "CONNECT";
const MESSAGE_EVENT: &str = "MESSAGE";
const DISCONNECT_EVENT: &str = "DISCONNECT";

/// The environment variable the lifecycle handlers read the connection they push to out of. It
/// is set by `UpdateFunctionConfiguration` once the watcher's own connection exists, because
/// the id of a connection is minted by the handshake that opens it.
const WATCHER_VARIABLE: &str = "WATCHER_CONNECTION_ID";

/// The service principal a gateway invokes a route's function as, and the action it performs:
/// without this grant on the function the invocation is never made.
const GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The action a `@connections` push asks for, which the execution role a lifecycle handler runs
/// as has to be allowed for its push to be served at all.
const MANAGE_CONNECTIONS: &str = "execute-api:ManageConnections";

/// The status RFC 6455 §4.2.2 requires of a completed opening handshake.
const SWITCHING_PROTOCOLS: u16 = 101;

/// The masking key the client frames below are masked with: the one RFC 6455 §5.7 publishes in
/// its own worked example. "All frames sent from the client to the server are masked" (§5.1),
/// so a server that does not unmask reads a different message than the one sent.
const MASKING_KEY: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

/// The close status a client ends a connection with, "indicat[ing] a normal closure" (RFC 6455
/// §7.4.1) — the clean close that makes the `$disconnect` call due.
const NORMAL_CLOSURE: u16 = 1000;

/// The largest frame payload this client reads: the 128 KiB API Gateway publishes as a WebSocket
/// api's message payload quota, which is more than any record below carries. The buffer a frame
/// is read into is sized from the length the PEER states, so that length is held to this before a
/// byte is allocated for it.
const MAX_FRAME_BYTES: usize = 128 * 1024;

/// How long a case waits for one frame, and for a function to report itself invocable: each
/// frame may be the answer of a function whose runtime this emulator has to start first, and
/// these cases run beside every other suite of the workspace, where a host carrying thousands of
/// them at once starts one runtime far slower than an idle one does. Both are therefore
/// GENEROUS, and both bound no behaviour any row asserts on — what is waited for is the frame, a
/// CONDITION, and nothing here asserts on the clock.
const FRAME_DEADLINE: Duration = Duration::from_secs(120);
const READY_BUDGET: Duration = Duration::from_secs(120);

/// The ONE handler every route of the api fronts, inline in the template exactly as a
/// synthesised app carries one.
///
/// It reports the call it was handed — the function that ran, the route key, the event type, the
/// connection id, and the marker the handshake's query string carried — and then answers the way
/// that KIND of route can answer a client: a message route returns the record as its body, which
/// a two-way route relays back down the socket, while a lifecycle route's answer reaches nobody,
/// so it pushes the record to the watcher connection its environment names through the
/// `@connections` endpoint its own event reports.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    reported = event.get("requestContext") or {}
    record = json.dumps(
        {
            "fn": context.function_name,
            "routeKey": reported.get("routeKey"),
            "eventType": reported.get("eventType"),
            "connectionId": reported.get("connectionId"),
            "marker": (event.get("queryStringParameters") or {}).get("marker"),
        }
    )
    if reported.get("eventType") in ("CONNECT", "DISCONNECT"):
        watcher = os.environ.get("WATCHER_CONNECTION_ID") or ""
        if watcher:
            boto3.client(
                "apigatewaymanagementapi",
                endpoint_url="https://%s/%s" % (reported["domainName"], reported["stage"]),
            ).post_to_connection(ConnectionId=watcher, Data=record.encode("utf-8"))
        return {"statusCode": 200}
    return {"statusCode": 200, "body": record}
"#;

/// CloudFormation, IAM, AWS Lambda, API Gateway v2 and the API Gateway Management API over ONE
/// engine, as `core/router/src/lib.rs` mounts them: the socket the api opens, the functions its
/// routes front and the connection registry a push is delivered through are all served by this
/// one emulator.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the handshakes below carry none, as a
    // WebSocket client of a published endpoint sends none.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One API Gateway v2 control-plane call, as `(status, parsed body)`.
async fn control(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", &host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/apigateway\
                 /aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One control-plane create that must answer the modelled `201`.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    uri: &str,
    body: Value,
) -> Value {
    let (status, answered) = control(router, "POST", uri, Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {what} must answer 201, got {status} {answered}"
    );
    answered
}

/// The text member `member` of `body`.
fn as_str(case: &str, what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no {member}: {body}"))
        .to_string()
}

/// The template the four functions are deployed as: the role IAM holds, and the one handler
/// above under each function's own name, so the record a call produces names the function the
/// gateway invoked.
fn function_stack() -> String {
    let function = |name: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Timeout": 30,
                "Code": { "ZipFile": HANDLER_SOURCE },
            },
        })
    };
    let arn = |logical: &str| json!({ "Value": { "Fn::GetAtt": [logical, "Arn"] } });
    json!({
        "Resources": {
            "Connect": function(CONNECT_FUNCTION),
            "Disconnect": function(DISCONNECT_FUNCTION),
            "Ping": function(PING_FUNCTION),
            "Default": function(DEFAULT_FUNCTION),
        },
        "Outputs": {
            "ConnectArn": arn("Connect"),
            "DisconnectArn": arn("Disconnect"),
            "PingArn": arn("Ping"),
            "DefaultArn": arn("Default"),
        },
    })
    .to_string()
}

/// Create the execution role the template names, where IAM holds it: trusted by Lambda, which is
/// what lets that service be handed the role at all, and carrying the ONE grant a handler that
/// pushes to a connection needs — `execute-api:ManageConnections` on the `@connections` route
/// within a stage, without which the push is refused and nothing is delivered.
async fn execution_role(router: &MultiServiceRouter) {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    // The api this role's functions push to is minted after the role, so the api and the stage
    // of the resource are named by wildcard; the ACTION and the `@connections` space are not.
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": MANAGE_CONNECTIONS,
            "Resource": format!(
                "arn:aws:execute-api:{REGION}:{account}:*/*/POST/@connections/*",
                account = engine::emulator_account_id(),
            ),
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "manage-the-connections"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `function` `Active` — "the function
/// is ready to be invoked" (`FunctionConfiguration$State`) — and no update in flight. The spans
/// are that service's own, so nothing here sleeps a fixed span on its behalf.
async fn wait_until_invocable(router: &MultiServiceRouter, case: &str, function: &str) {
    let deadline = Instant::now() + READY_BUDGET;
    loop {
        let (_status, read) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        let active = read.get("State").and_then(Value::as_str) == Some("Active");
        let settled = read
            .get("LastUpdateStatus")
            .and_then(Value::as_str)
            .is_none_or(|status| status != "InProgress");
        if active && settled {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {function} invocable, last read {read}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Grant API Gateway the invocation of `function` — the `AddPermission` a client makes beside
/// the api whose route fronts it.
async fn gateway_may_invoke(router: &MultiServiceRouter, case: &str, function: &str) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": "apigateway-may-invoke",
            "Action": INVOKE_ACTION,
            "Principal": GATEWAY_PRINCIPAL,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission on {function} answered {status} {granted}"
    );
}

/// Deploy the four functions as one stack, grant the gateway each invocation, and answer their
/// ARNs under the output names above. A template is what archives inline source into a deployment
/// package this emulator can run, which is how a handler written out here comes to be a function
/// that RUNS.
async fn given_functions(
    router: &MultiServiceRouter,
    case: &str,
) -> std::collections::BTreeMap<String, String> {
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &function_stack())],
    )
    .await;
    let (status, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: expected the functions' stack to deploy, got {status} with {described}"
    );
    let outputs = outputs_of(&described);
    for function in [
        CONNECT_FUNCTION,
        DISCONNECT_FUNCTION,
        PING_FUNCTION,
        DEFAULT_FUNCTION,
    ] {
        wait_until_invocable(router, case, function).await;
        gateway_may_invoke(router, case, function).await;
    }
    outputs
}

/// The ARN the stack published under `output`.
fn published_arn(
    case: &str,
    outputs: &std::collections::BTreeMap<String, String>,
    output: &str,
) -> String {
    outputs
        .get(output)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no {output}: {outputs:?}"))
}

/// Point the two lifecycle functions at `connection_id`: the connection their push is addressed
/// to, which only exists once a client has opened it.
async fn point_lifecycle_at(router: &MultiServiceRouter, case: &str, connection_id: &str) {
    for function in [CONNECT_FUNCTION, DISCONNECT_FUNCTION] {
        let (status, updated) = lambda(
            router,
            "PUT",
            &format!("/2015-03-31/functions/{function}/configuration"),
            Some(json!({
                "Environment": { "Variables": { WATCHER_VARIABLE: connection_id } },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateFunctionConfiguration on {function} answered {status} {updated}"
        );
        wait_until_invocable(router, case, function).await;
    }
}

/// A deployed WEBSOCKET api whose four routes each front their own function, two of them two-way.
/// Answers the `ApiEndpoint` it announces, which is the address a client dials.
async fn given_api(
    router: &MultiServiceRouter,
    case: &str,
    outputs: &std::collections::BTreeMap<String, String>,
) -> String {
    let api = created(
        router,
        case,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": "ws-lifecycle-watcher",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action",
        }),
    )
    .await;
    let api_id = as_str(case, "CreateApi", &api, "apiId");
    let announced = as_str(case, "CreateApi", &api, "apiEndpoint");

    // The lifecycle routes are ONE-WAY — their answer reaches no client — and the two message
    // routes are two-way, which is what makes the integration's answer travel back down the
    // socket the frame came from.
    for (route_key, output, two_way) in [
        (CONNECT_ROUTE, "ConnectArn", false),
        (DISCONNECT_ROUTE, "DisconnectArn", false),
        (PING_ROUTE, "PingArn", true),
        (DEFAULT_ROUTE, "DefaultArn", true),
    ] {
        let integration = created(
            router,
            case,
            "CreateIntegration",
            &format!("/v2/apis/{api_id}/integrations"),
            json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": published_arn(case, outputs, output),
                "integrationMethod": "POST",
            }),
        )
        .await;
        let integration_id = as_str(case, "CreateIntegration", &integration, "integrationId");
        let route = created(
            router,
            case,
            "CreateRoute",
            &format!("/v2/apis/{api_id}/routes"),
            json!({
                "routeKey": route_key,
                "target": format!("integrations/{integration_id}"),
            }),
        )
        .await;
        if two_way {
            let route_id = as_str(case, "CreateRoute", &route, "routeId");
            created(
                router,
                case,
                "CreateRouteResponse",
                &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
                json!({ "routeResponseKey": "$default" }),
            )
            .await;
        }
    }
    created(
        router,
        case,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": STAGE }),
    )
    .await;
    created(
        router,
        case,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
    )
    .await;
    announced
}

/// One client's end of a connection: the socket an opening handshake was completed on, and the
/// frames written and read over it.
///
/// The frames are written and read here rather than through a WebSocket client library because
/// this crate's test target has none; what each one carries is RFC 6455 §5's, cited where it is
/// built.
struct Client {
    stream: TcpStream,
}

impl Client {
    /// Complete an RFC 6455 opening handshake against `path` of the announced socket, asserting
    /// the `101` and the `Sec-WebSocket-Accept` the offered key owes.
    async fn open(case: &str, announced: &str, path: &str) -> Self {
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
            ("Sec-WebSocket-Key", RFC_EXAMPLE_KEY),
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
        let head = tokio::time::timeout(FRAME_DEADLINE, read_head(&mut stream))
            .await
            .unwrap_or_else(|_elapsed| {
                panic!("case {case:?}: the handshake at {path:?} must be answered a head")
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: the socket answered no readable head for {path:?}")
            });
        let (line, fields) = head.split_once("\r\n").unwrap_or((head.as_str(), ""));
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("case {case:?}: no status code in {line:?}"));
        assert_eq!(
            status, SWITCHING_PROTOCOLS,
            "case {case:?}: the handshake at {path:?} must be completed, got {head:?}"
        );
        let accept = fields
            .split("\r\n")
            .filter_map(|field| field.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case(ACCEPT_FIELD))
            .map(|(_, value)| value.trim().to_string());
        assert_eq!(
            accept.as_deref(),
            Some(RFC_EXAMPLE_ACCEPT),
            "case {case:?}: the completed handshake must carry the accept value the offered key \
             owes, got {head:?}"
        );
        Self { stream }
    }

    /// Write `message` as one masked text frame, the way a client sends a message.
    async fn send(&mut self, case: &str, message: &str) {
        let frame = masked(case, TEXT_OPCODE, message.as_bytes());
        self.write(case, &frame).await;
    }

    /// The next text frame this socket is sent, read as the JSON record a function built.
    async fn record(&mut self, case: &str, what: &str) -> Value {
        let frame = tokio::time::timeout(FRAME_DEADLINE, self.text(case, what))
            .await
            .unwrap_or_else(|_elapsed| {
                panic!(
                    "case {case:?}: {what} did not arrive within {FRAME_DEADLINE:?}, so the call \
                     it reports was never dispatched"
                )
            });
        serde_json::from_str(&frame).unwrap_or_else(|error| {
            panic!("case {case:?}: {what} must be a function's own record, got {frame:?} ({error})")
        })
    }

    /// Read frames off the socket until one carries the DATA this reader is waiting for, as the
    /// text it carries.
    ///
    /// A CONTROL frame may be interleaved ahead of it "in the middle of a fragmented message"
    /// (RFC 6455 §5.4), so one is handled rather than read as the answer: a ping is answered
    /// with the pong it owes (§5.5.3) and the wait goes on, a pong is nothing to do, and
    /// anything else — a close above all — ends the wait, because the answer cannot arrive on a
    /// connection that is being closed.
    async fn text(&mut self, case: &str, what: &str) -> String {
        loop {
            let (opcode, final_frame, payload) = self.frame(case, what).await;
            match opcode {
                TEXT_OPCODE => {
                    assert!(
                        final_frame,
                        "case {case:?}: {what} is small enough to arrive as one frame"
                    );
                    return String::from_utf8(payload).unwrap_or_else(|error| {
                        panic!(
                            "case {case:?}: {what} must be the text its opcode says it is, got \
                             {error}"
                        )
                    });
                }
                PING_OPCODE => {
                    let pong = masked(case, PONG_OPCODE, &payload);
                    self.write(case, &pong).await;
                }
                PONG_OPCODE => {}
                other => panic!(
                    "case {case:?}: {what} must arrive as a text frame, got opcode {other} with \
                     {:?}",
                    String::from_utf8_lossy(&payload)
                ),
            }
        }
    }

    /// One frame off the socket: its opcode, whether it ends a message, and its payload.
    async fn frame(&mut self, case: &str, what: &str) -> (u8, bool, Vec<u8>) {
        let head = self.exactly(case, what, 2).await;
        let final_frame = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0f;
        assert_eq!(
            head[1] & 0x80,
            0,
            "case {case:?}: a frame a server sends is never masked (RFC 6455 §5.1), got {head:?}"
        );
        // The payload length: the 7 bits themselves below 126, two further bytes at 126 and
        // eight at 127, each unsigned and in network byte order (RFC 6455 §5.2).
        let length = match head[1] & 0x7f {
            126 => {
                let extended = self.exactly(case, what, 2).await;
                usize::from(u16::from_be_bytes([extended[0], extended[1]]))
            }
            127 => {
                let extended = self.exactly(case, what, 8).await;
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&extended);
                usize::try_from(u64::from_be_bytes(bytes)).unwrap_or_else(|_| {
                    panic!("case {case:?}: {what} states a length this host cannot hold")
                })
            }
            short => usize::from(short),
        };
        assert!(
            length <= MAX_FRAME_BYTES,
            "case {case:?}: {what} states a payload of {length} bytes, past the {MAX_FRAME_BYTES} \
             this plane ever sends"
        );
        let payload = self.exactly(case, what, length).await;
        (opcode, final_frame, payload)
    }

    /// Exactly `count` bytes off the socket.
    async fn exactly(&mut self, case: &str, what: &str, count: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; count];
        self.stream
            .read_exact(&mut buffer)
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: {what} must carry {count} more bytes, got {error}")
            });
        buffer
    }

    /// Close the client's end cleanly, which is what makes the `$disconnect` call due.
    async fn close(mut self, case: &str) {
        // The status the close carries in the first two payload bytes (RFC 6455 §5.5.1).
        let frame = masked(case, CLOSE_OPCODE, &NORMAL_CLOSURE.to_be_bytes());
        self.write(case, &frame).await;
        // Then whatever the server answers is DRAINED until the connection ends: "the server
        // MUST send a Close frame in response" (RFC 6455 §5.5.1), and a socket dropped with
        // that answer still unread reaches the peer as a reset on some platforms rather than as
        // the clean close this row drives. Nothing is asserted on it — what the row asserts on
        // is the `$disconnect` record the watcher is pushed — so a connection the server ends
        // without answering is read here as the end it is.
        let _drained = tokio::time::timeout(FRAME_DEADLINE, async {
            let mut scratch = [0u8; 1024];
            while let Ok(read) = self.stream.read(&mut scratch).await {
                if read == 0 {
                    break;
                }
            }
        })
        .await;
    }

    /// Write `bytes` and flush them, so the server sees the frame as soon as it is sent.
    async fn write(&mut self, case: &str, bytes: &[u8]) {
        self.stream
            .write_all(bytes)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be sent, got {error}"));
        self.stream
            .flush()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be flushed, got {error}"));
    }
}

/// The opcodes the frames below carry: the text a message is sent as, and the three control
/// opcodes a connection's own traffic uses (RFC 6455 §5.2).
const TEXT_OPCODE: u8 = 0x1;
const CLOSE_OPCODE: u8 = 0x8;
const PING_OPCODE: u8 = 0x9;
const PONG_OPCODE: u8 = 0xa;

/// One whole frame a CLIENT sends, as RFC 6455 §5.2 lays it out: `FIN` set with `opcode`, then
/// the mask bit with the payload length, the masking key, and the payload masked with it
/// (§5.3) — "all frames sent from the client to the server" are masked (§5.1).
///
/// Every payload a case sends is short enough for the 7-bit length, which is asserted rather
/// than assumed so a longer one fails here instead of being framed wrongly.
fn masked(case: &str, opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() < 126,
        "case {case:?}: these payloads are short enough for a 7-bit length, got {} bytes",
        payload.len()
    );
    let mut frame = vec![0x80 | opcode, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&MASKING_KEY);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(at, byte)| byte ^ MASKING_KEY[at % MASKING_KEY.len()]),
    );
    frame
}

/// Assert `record` is the one the call it describes owes: the function that ran, the route key,
/// the event type and the connection id.
fn assert_record(
    case: &str,
    what: &str,
    record: &Value,
    function: &str,
    route_key: &str,
    event_type: &str,
    connection_id: &str,
) {
    for (member, expected) in [
        ("fn", function),
        ("routeKey", route_key),
        ("eventType", event_type),
        ("connectionId", connection_id),
    ] {
        let actual = record.get(member).and_then(Value::as_str);
        assert_eq!(
            actual,
            Some(expected),
            "case {case:?}: {what} must report {member} {expected:?}, got {actual:?} ({record})"
        );
    }
}

/// The connection id `record` reports, having checked it is one.
fn connection_id_of(case: &str, what: &str, record: &Value) -> String {
    let id = record
        .get("connectionId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {what} reports no connectionId: {record}"));
    assert!(
        !id.is_empty(),
        "case {case:?}: {what} must report the connection id the handshake minted, got an empty \
         one ({record})"
    );
    id.to_string()
}

/// One row: what the client carries in its handshake, and the frame it sends once it is open.
struct Row {
    case: &'static str,
    /// The `marker` the handshake's query string carries, which the connection request's event
    /// reports under `queryStringParameters`.
    marker: &'static str,
    /// The message the client sends, with the route the api's selection expression must pick for
    /// it and the function that route fronts — `None` for a connection that sends nothing at all.
    message: Option<(&'static str, &'static str, &'static str)>,
}

/// The rows span what the rule generalises over: a frame naming a route the api holds, a frame
/// naming none (served by the catch-all the api declares), and no frame at all, each under its own
/// marker.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: a client whose frame names the `ping` route.
            case: "a connection whose frame names a route of the api",
            marker: "alpha",
            message: Some((r#"{"action":"ping"}"#, PING_ROUTE, PING_FUNCTION)),
        },
        Row {
            case: "a connection whose frame names no route, served by the catch-all",
            marker: "beta",
            message: Some((
                r#"{"action":"nothing-matches"}"#,
                DEFAULT_ROUTE,
                DEFAULT_FUNCTION,
            )),
        },
        Row {
            case: "a connection that opens and closes without sending anything",
            marker: "gamma",
            message: None,
        },
    ]
}

/// RULE: the three calls of a connection are each served by their own route's integration, under
/// the one connection id that connection was minted, and a lifecycle route's push reaches another
/// live connection of the stage.
///
/// On the MULTI-THREADED flavour, as the sibling suite that drives this socket is: the api's own
/// listener task, the runtimes of the functions its routes front and this test's own clients all
/// have to make progress at the same time, and a blocking step on the serving path would
/// otherwise stall the whole case.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_connections_lifecycle_calls_reach_their_own_routes_function() {
    let case = "the $connect/message/$disconnect lifecycle of a WebSocket connection";
    let router = emulator();
    execution_role(&router).await;
    let outputs = given_functions(&router, case).await;
    let announced = given_api(&router, case, &outputs).await;

    // The WATCHER: a client of the same stage, whose own connection id is read back off the reply
    // of a two-way route — the only way a client learns the id the handshake minted for it.
    let mut watcher = Client::open(case, &announced, &format!("/{STAGE}?marker=watcher")).await;
    watcher.send(case, r#"{"action":"ping"}"#).await;
    let replied = watcher.record(case, "the watcher's own ping reply").await;
    let watcher_id = connection_id_of(case, "the watcher's own ping reply", &replied);
    assert_record(
        case,
        "the watcher's own ping reply",
        &replied,
        PING_FUNCTION,
        PING_ROUTE,
        MESSAGE_EVENT,
        &watcher_id,
    );
    assert_eq!(
        replied.get("marker").and_then(Value::as_str),
        None,
        "case {case:?}: a message's event carries no query string, only the connection request's \
         does ({replied})"
    );

    // The lifecycle handlers' answers reach no client, so they are pointed at the watcher: from
    // here on, every CONNECT and DISCONNECT call is observable on the watcher's own socket.
    point_lifecycle_at(&router, case, &watcher_id).await;

    let rows = rows();
    let opened = rows.len() + 1;
    let mut minted: BTreeSet<String> = BTreeSet::new();
    minted.insert(watcher_id.clone());
    for row in rows {
        let case = row.case;
        let mut client =
            Client::open(case, &announced, &format!("/{STAGE}?marker={}", row.marker)).await;

        let connected = watcher.record(case, "the $connect record").await;
        let id = connection_id_of(case, "the $connect record", &connected);
        assert_record(
            case,
            "the $connect record",
            &connected,
            CONNECT_FUNCTION,
            CONNECT_ROUTE,
            CONNECT_EVENT,
            &id,
        );
        assert_eq!(
            connected.get("marker").and_then(Value::as_str),
            Some(row.marker),
            "case {case:?}: the connection request's event must carry the handshake's query \
             string ({connected})"
        );
        assert!(
            minted.insert(id.clone()),
            "case {case:?}: every connection of the stage is minted its own id, and {id:?} was \
             already held by another one of {minted:?}"
        );

        if let Some((message, route_key, function)) = row.message {
            client.send(case, message).await;
            let replied = client.record(case, "the two-way route's reply").await;
            assert_record(
                case,
                "the two-way route's reply",
                &replied,
                function,
                route_key,
                MESSAGE_EVENT,
                &id,
            );
        }
        client.close(case).await;

        // The very next frame the watcher is sent is this connection's DISCONNECT record: a
        // message route's answer went back to the client that sent it, so nothing of this row's
        // traffic reached the watcher in between.
        let disconnected = watcher.record(case, "the $disconnect record").await;
        assert_record(
            case,
            "the $disconnect record",
            &disconnected,
            DISCONNECT_FUNCTION,
            DISCONNECT_ROUTE,
            DISCONNECT_EVENT,
            &id,
        );
    }

    assert_eq!(
        minted.len(),
        opened,
        "case {case:?}: the watcher and every row's client must each hold their own connection \
         id, got {minted:?}"
    );
    watcher.close(case).await;
}
