// SPDX-License-Identifier: CC0-1.0

//! A WEBSOCKET api's routes served by the FUNCTIONS they front, over a real connection: the
//! `$connect` / `$disconnect` lifecycle, the route each frame is dispatched to, and the
//! `REQUEST` authorizer that decides the handshake.
//!
//! The rule every table below is one slice of: **each call of a WebSocket connection is
//! served by performing the integration of the route that call belongs to — the `$connect`
//! route for the opening handshake, the route the api's `RouteSelectionExpression` selects
//! for each frame, the `$disconnect` route for the close — so an `AWS_PROXY` integration's
//! answer is the answer of the function it names, invoked with the event that call is
//! (`requestContext.eventType` `CONNECT` / `MESSAGE` / `DISCONNECT`, the route's own
//! `routeKey`, and the one `connectionId` the handshake minted, with the handshake's query
//! string on the connection request), while the handshake itself completes only on a `2xx`
//! from the connection request and a `REQUEST` authorizer in front of it decides it first:
//! `401` when an identity source is absent (the authorizer not invoked at all), `403` when
//! its function denies, and otherwise the context it returned reaches every later call's
//! event. Every one of those invocations is made by the gateway as its OWN service principal
//! (`apigateway.amazonaws.com`), so it is made only where the FUNCTION's resource-based policy
//! allows that principal `lambda:InvokeFunction` with the route's own `execute-api` ARN
//! (`{api-id}/{stage}/{route-key}`) as the `aws:SourceArn` such a grant may be narrowed by: an
//! invocation the policy does not admit is NEVER MADE, and the call produced no reply — the
//! handshake refused `500` for the connection request, the gateway's own
//! `Internal server error` frame for a message.**
//!
//! Nothing here is a value this emulator fills in: every assertion is on something a
//! function RAN and returned, which is why AWS Lambda is mounted beside this service on one
//! engine, and why the lifecycle routes — whose answers reach no client — record the event
//! they were given in an Amazon SQS queue, through the AWS SDK their runtime supplies, the
//! way a `$connect` handler stores a connection id.
//!
//! Three slices of that rule are about the DISPATCH DECISION reaching a terminal outcome, and
//! two of them are about what did NOT happen, which only what RAN can show:
//!
//! > The authorizer of the route serving the connection request decides it BEFORE that route's
//! > integration is performed, so a DENY refuses the handshake and leaves the `$connect`
//! > integration unperformed, while an ALLOW completes it and the integration runs exactly
//! > once. A frame the api routes NOWHERE — nothing selected and no `$default` route declared —
//! > reaches no integration at all and is answered by the gateway's own `Forbidden` frame, over
//! > a connection that keeps serving the routes it does hold. And the api's CURRENT
//! > `RouteSelectionExpression` is the path a deployed stage selects by, so a frame sent after
//! > that expression was replaced and the stage redeployed reaches the route the NEW path
//! > addresses — and one keyed the way the old path named reaches whatever the new path selects,
//! > which may be nothing.
//!
//! Each of those is an outcome a client can act on — a refused upgrade, a frame that reached one
//! handler, a frame that reached none while the connection stayed open — and never an
//! unanswered wait.
//!
//! The suites that own the steps BEFORE the integration are not restated here: the
//! handshake's own statuses and the `MOCK` reply rule are
//! `websocket_transport_data_plane.rs`, and the identity-source precondition over every
//! spelling it takes is `websocket_connect_authorization.rs`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    gateway_may_invoke, given_function, given_ungranted_function, granted, revoked,
};
use crate::websocket_transport_data_plane::{
    exchanged_reply, handshake_at, ClientSocket, ACTION_SELECTION_EXPRESSION,
};

/// The Region every control-plane request below is signed for.
const REGION: &str = "us-east-1";

/// The stage every api below is deployed to.
const STAGE: &str = "prod";

/// The statuses a refused handshake carries: the `403` an authorizer's DENY earns and the
/// `401` an absent identity source earns.
const FORBIDDEN: u16 = 403;
const UNAUTHORIZED: u16 = 401;

/// The status the gateway answers for a connection request whose authorizer it could not get
/// a decision from at all — which a loaded host can produce transiently while the
/// authorizer's runtime is starting, so a row expecting another status offers the handshake
/// again until that decision arrives ([`DECISION_WAIT`]).
const INTERNAL_SERVER_ERROR: u16 = 500;

/// How long such a row keeps offering the handshake: generous, and bounding no behaviour the
/// rows assert on.
const DECISION_WAIT: Duration = Duration::from_secs(120);

/// How long a case waits for a recorded event before failing, and how often it looks: a
/// lifecycle route's own answer reaches no client, so the record it wrote is read by polling
/// the queue.
const RECORD_WAIT: Duration = Duration::from_secs(20);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// This service, AWS Lambda and Amazon SQS on ONE engine, with request authentication off.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One control-plane request against THIS service, as its status and parsed body.
async fn control(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("the body serializes")),
        None => bytes::Bytes::new(),
    };
    let request = smithy_server_runtime::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(payload)
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One control-plane request that must answer `expected`.
async fn created(
    emulator: &MultiServiceRouter,
    case: &str,
    what: &str,
    uri: &str,
    body: Value,
) -> Value {
    let (status, answered) = control(emulator, "POST", uri, Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {what} must answer 201, got {status} {answered}"
    );
    answered
}

/// One Amazon SQS call that must answer 200.
async fn sqs(emulator: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("authorization", authorization("sqs"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: SQS {operation} must answer 200, got {status} {parsed}"
    );
    parsed
}

/// The queue the lifecycle routes record the event they were given in.
async fn given_queue(emulator: &MultiServiceRouter, case: &str) -> String {
    let created = sqs(
        emulator,
        case,
        "CreateQueue",
        json!({ "QueueName": "websocket-route-records" }),
    )
    .await;
    created
        .get("QueueUrl")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue answered no QueueUrl: {created}"))
        .to_string()
}

/// A handler that records the event it was given in the queue and answers `reply`.
///
/// It is the pattern a real `$connect` handler follows — the connection id it is given is
/// what it stores — and it is how a call whose answer reaches no client is observed here.
fn recording_handler(name: &str, queue_url: &str, reply: &str) -> String {
    format!(
        "import json\n\
         import boto3\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   boto3.client(\"sqs\").send_message(\n\
         \x20       QueueUrl={queue:?},\n\
         \x20       MessageBody=json.dumps({{\"fn\": {name:?}, \"event\": event}}),\n\
         \x20   )\n\
         \x20   return {reply}\n",
        queue = queue_url,
        name = name,
        reply = reply,
    )
}

/// A handler that answers the event it was given as its response body, which is how a
/// two-way route's own event is asserted.
fn echoing_handler(name: &str) -> String {
    format!(
        "import json\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   return {{\"statusCode\": 200,\n\
         \x20           \"body\": json.dumps({{\"fn\": {name:?}, \"event\": event}})}}\n",
        name = name,
    )
}

/// A `REQUEST` authorizer's function: it records the event it was given and answers the
/// policy document the handshake's own `token` asks for, with a context for the routes.
fn authorizer_handler(queue_url: &str) -> String {
    format!(
        "import json\n\
         import boto3\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   boto3.client(\"sqs\").send_message(\n\
         \x20       QueueUrl={queue:?},\n\
         \x20       MessageBody=json.dumps({{\"fn\": \"authorizer\", \"event\": event}}),\n\
         \x20   )\n\
         \x20   token = (event.get(\"queryStringParameters\") or {{}}).get(\"token\", \"\")\n\
         \x20   effect = \"Allow\" if token == \"open\" else \"Deny\"\n\
         \x20   return {{\n\
         \x20       \"principalId\": \"connection-principal\",\n\
         \x20       \"policyDocument\": {{\n\
         \x20           \"Version\": \"2012-10-17\",\n\
         \x20           \"Statement\": [{{\n\
         \x20               \"Action\": \"execute-api:Invoke\",\n\
         \x20               \"Effect\": effect,\n\
         \x20               \"Resource\": event[\"methodArn\"],\n\
         \x20           }}],\n\
         \x20       }},\n\
         \x20       \"context\": {{\"tier\": \"gold\", \"token\": token}},\n\
         \x20   }}\n",
        queue = queue_url,
    )
}

/// The same authorizer answering in the OTHER format a Lambda authorizer states a decision
/// in: the boolean, "instead of an IAM policy".
fn simple_authorizer_handler(queue_url: &str) -> String {
    format!(
        "import json\n\
         import boto3\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   boto3.client(\"sqs\").send_message(\n\
         \x20       QueueUrl={queue:?},\n\
         \x20       MessageBody=json.dumps({{\"fn\": \"authorizer\", \"event\": event}}),\n\
         \x20   )\n\
         \x20   token = (event.get(\"queryStringParameters\") or {{}}).get(\"token\", \"\")\n\
         \x20   return {{\n\
         \x20       \"isAuthorized\": token == \"open\",\n\
         \x20       \"context\": {{\"tier\": \"gold\", \"token\": token}},\n\
         \x20   }}\n",
        queue = queue_url,
    )
}

/// One api, created with the socket its `ApiEndpoint` announces.
struct Api {
    api_id: String,
    /// The `host:port` of the api's own socket, read off the announced `ApiEndpoint` so a
    /// case dials the address the service reported.
    authority: String,
}

/// Create a WEBSOCKET api whose frames select a route by their `action` member.
async fn given_api(emulator: &MultiServiceRouter, case: &str) -> Api {
    given_selecting_api(emulator, case, ACTION_SELECTION_EXPRESSION).await
}

/// [`given_api`] for an api whose `RouteSelectionExpression` is `expression` — the PATH into
/// each message whose value is the key that message is routed by.
///
/// This suite creates its apis through its OWN control-plane caller ([`control`]) rather than
/// through `websocket_transport_data_plane`'s `create_selecting_api`, because every request of a
/// case here — the api, the functions its routes front and the grants that admit the gateway —
/// is signed for [`REGION`], the Region those functions live in.
async fn given_selecting_api(
    emulator: &MultiServiceRouter,
    case: &str,
    expression: &str,
) -> Api {
    let created = created(
        emulator,
        case,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": "socket-routes-api",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": expression
        }),
    )
    .await;
    let announced = created
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
        })
        .to_string();
    Api {
        api_id: created
            .get("apiId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        authority,
    }
}

/// Bind `function_arn` as an `AWS_PROXY` integration of `api_id` and route `route_key` to
/// it, with a `RouteResponse` when `two_way` — "to define a two-way route, create a route
/// response".
async fn given_route(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    route_key: &str,
    function_arn: &str,
    two_way: bool,
    authorizer_id: Option<&str>,
) {
    let integration = created(
        emulator,
        case,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_arn,
            "integrationMethod": "POST"
        }),
    )
    .await;
    let integration_id = integration
        .get("integrationId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut body = json!({
        "routeKey": route_key,
        "target": format!("integrations/{integration_id}")
    });
    if let Some(authorizer_id) = authorizer_id {
        // `CUSTOM` is the `AuthorizationType` "for using a Lambda authorizer".
        body["authorizationType"] = json!("CUSTOM");
        body["authorizerId"] = json!(authorizer_id);
    }
    let route = created(
        emulator,
        case,
        "CreateRoute",
        &format!("/v2/apis/{api_id}/routes"),
        body,
    )
    .await;
    if two_way {
        let route_id = route
            .get("routeId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        created(
            emulator,
            case,
            "CreateRouteResponse",
            &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
            json!({ "routeResponseKey": "$default" }),
        )
        .await;
    }
}

/// Create the stage every case dials and release a deployment to it.
async fn given_stage(emulator: &MultiServiceRouter, case: &str, api_id: &str) {
    created(
        emulator,
        case,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": STAGE }),
    )
    .await;
    created(
        emulator,
        case,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
    )
    .await;
}

/// Create a `REQUEST` authorizer over the handshake's `token` query parameter and answer its
/// `AuthorizerId`.
async fn given_authorizer(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    function_arn: &str,
) -> String {
    // The form `AuthorizerUri` documents: "arn:aws:apigateway:{region}:lambda:path/
    // 2015-03-31/functions/{function-arn}/invocations".
    let uri = format!(
        "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/invocations"
    );
    let authorizer = created(
        emulator,
        case,
        "CreateAuthorizer",
        &format!("/v2/apis/{api_id}/authorizers"),
        json!({
            "name": "connect-auth",
            "authorizerType": "REQUEST",
            "authorizerUri": uri,
            "identitySource": ["route.request.querystring.token"],
            // No caching, so each handshake of a case is decided by its own invocation.
            "authorizerResultTtlInSeconds": 0
        }),
    )
    .await;
    authorizer
        .get("authorizerId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateAuthorizer answered no AuthorizerId"))
        .to_string()
}

/// The records written to `queue_url` so far, drained from it: each is what one handler
/// recorded, `{"fn": …, "event": …}`.
async fn drained(emulator: &MultiServiceRouter, case: &str, queue_url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received
        .get("Messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| message.get("Body").and_then(Value::as_str))
                .filter_map(|body| serde_json::from_str::<Value>(body).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until a record whose `fn` is `name` has been written, and answer the event it
/// carried; panic when none arrives within [`RECORD_WAIT`].
///
/// A function's run is started by this emulator and is not instantaneous, so the wait is for
/// a record the CALL already owes rather than for anything a clock schedules.
async fn recorded_event(
    emulator: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    name: &str,
) -> Value {
    let deadline = std::time::Instant::now() + RECORD_WAIT;
    let mut seen: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline {
        for record in drained(emulator, case, queue_url).await {
            let recorded = record
                .get("fn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if recorded == name {
                return record.get("event").cloned().unwrap_or(Value::Null);
            }
            seen.push(recorded);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: expected a record from {name:?}, saw records from {seen:?}");
}

/// The member of `event` at `pointer`, as JSON.
fn member(event: &Value, pointer: &str) -> Value {
    event.pointer(pointer).cloned().unwrap_or(Value::Null)
}

#[tokio::test]
async fn each_call_of_a_connection_is_served_by_its_own_routes_function() {
    let case = "a connection whose $connect, $disconnect and two message routes each front \
                their own function";
    let (emulator, engine) = emulator();
    let queue = given_queue(&emulator, case).await;

    // One function per route key, as a client provisions them: the lifecycle routes record
    // the event they were given, and the message routes answer it.
    let connect = given_function(
        &emulator,
        &engine,
        case,
        "connect-route",
        &recording_handler("$connect", &queue, "{\"statusCode\": 200}"),
    )
    .await;
    let disconnect = given_function(
        &emulator,
        &engine,
        case,
        "disconnect-route",
        &recording_handler("$disconnect", &queue, "{\"statusCode\": 200}"),
    )
    .await;
    let ping = given_function(&emulator, &engine, case, "ping-route", &echoing_handler("ping"))
        .await;
    let sendmsg = given_function(
        &emulator,
        &engine,
        case,
        "sendmsg-route",
        &echoing_handler("sendmsg"),
    )
    .await;

    let api = given_api(&emulator, case).await;
    // No `$default` route: an action no route matches is the api's own refusal.
    for (route_key, function, two_way) in [
        ("$connect", &connect, false),
        ("$disconnect", &disconnect, false),
        ("ping", &ping, true),
        ("sendmsg", &sendmsg, true),
    ] {
        given_route(
            &emulator,
            case,
            &api.api_id,
            route_key,
            function,
            two_way,
            None,
        )
        .await;
    }
    given_stage(&emulator, case, &api.api_id).await;

    // The handshake: completed on the `$connect` function's own 2xx, with the query string
    // the client dialled with.
    let mut socket: ClientSocket = handshake_at(
        case,
        &api.authority,
        &format!("/{STAGE}?token=open&trace=abc"),
    )
    .await
    .unwrap_or_else(|status| {
        panic!("case {case:?}: the handshake must be completed, got {status}")
    });

    let connect_event = recorded_event(&emulator, case, &queue, "$connect").await;
    let connection_id = member(&connect_event, "/requestContext/connectionId");
    for (what, pointer, expected) in [
        (
            "the kind of call",
            "/requestContext/eventType",
            json!("CONNECT"),
        ),
        (
            "the route that served it",
            "/requestContext/routeKey",
            json!("$connect"),
        ),
        ("the stage", "/requestContext/stage", json!(STAGE)),
        (
            "the handshake's query string",
            "/queryStringParameters/token",
            json!("open"),
        ),
        (
            "every parameter of it",
            "/queryStringParameters/trace",
            json!("abc"),
        ),
    ] {
        assert_eq!(
            member(&connect_event, pointer),
            expected,
            "case {case:?}: expected {what} at {pointer} to be {expected}, got \
             {connect_event}"
        );
    }
    assert!(
        connection_id
            .as_str()
            .is_some_and(|id| !id.trim().is_empty()),
        "case {case:?}: the connection request must carry the minted connectionId, got \
         {connect_event}"
    );

    // Each frame: dispatched by the route selection expression to that action's own
    // function, under the SAME connection, and answered with what that function returned.
    //
    // The rows span what the dispatch generalises over: each action with a route of its
    // own, an action no route matches on an api with no `$default` route, a frame naming no
    // action at all, and a frame that is no JSON object.
    let cases: [(&str, &str, Option<&str>); 5] = [
        ("the first action", r#"{"action":"ping"}"#, Some("ping")),
        (
            "another action, which is another function",
            r#"{"action":"sendmsg","text":"hello"}"#,
            Some("sendmsg"),
        ),
        (
            "the first action again, on the same connection",
            r#"{"action":"ping","again":true}"#,
            Some("ping"),
        ),
        ("an action no route matches", r#"{"action":"nope"}"#, None),
        ("a frame that is no JSON object", "not json at all", None),
    ];
    for (row, frame, expected) in cases {
        let answer = exchanged_reply(case, &mut socket, frame).await;
        let answered: Value = serde_json::from_str(&answer).unwrap_or_else(|error| {
            panic!("case {case:?} / {row}: expected a JSON frame, got {answer:?}: {error}")
        });
        match expected {
            Some(name) => {
                assert_eq!(
                    member(&answered, "/fn"),
                    json!(name),
                    "case {case:?} / {row}: expected {name:?}'s own function to answer, got \
                     {answered}"
                );
                for (what, pointer, expected) in [
                    (
                        "the kind of call",
                        "/event/requestContext/eventType",
                        json!("MESSAGE"),
                    ),
                    (
                        "the route that served it",
                        "/event/requestContext/routeKey",
                        json!(name),
                    ),
                    (
                        "the connection it arrived on",
                        "/event/requestContext/connectionId",
                        connection_id.clone(),
                    ),
                    ("the frame itself", "/event/body", json!(frame)),
                ] {
                    assert_eq!(
                        member(&answered, pointer),
                        expected,
                        "case {case:?} / {row}: expected {what} at {pointer} to be \
                         {expected}, got {answered}"
                    );
                }
            }
            None => {
                // An action the api routes nowhere: the gateway's own frame, naming the
                // connection it is about — no function of the api is reached at all.
                assert_eq!(
                    member(&answered, "/message"),
                    json!("Forbidden"),
                    "case {case:?} / {row}: expected the gateway's Forbidden frame, got \
                     {answered}"
                );
                assert_eq!(
                    member(&answered, "/connectionId"),
                    connection_id.clone(),
                    "case {case:?} / {row}: the gateway's frame names the connection, got \
                     {answered}"
                );
            }
        }
    }

    // The close: the `$disconnect` route's own function, with the same connection.
    drop(socket);
    let disconnect_event = recorded_event(&emulator, case, &queue, "$disconnect").await;
    for (what, pointer, expected) in [
        (
            "the kind of call",
            "/requestContext/eventType",
            json!("DISCONNECT"),
        ),
        (
            "the route that served it",
            "/requestContext/routeKey",
            json!("$disconnect"),
        ),
        (
            "the connection that ended",
            "/requestContext/connectionId",
            connection_id.clone(),
        ),
    ] {
        assert_eq!(
            member(&disconnect_event, pointer),
            expected,
            "case {case:?}: expected {what} at {pointer} to be {expected}, got \
             {disconnect_event}"
        );
    }
}

#[tokio::test]
async fn the_connection_requests_own_status_decides_the_handshake() {
    /// What one row expects of the handshake: completed, or refused with this status and
    /// the body the connection request's own function returned.
    type Expected = Result<(), (u16, &'static str)>;
    // The rows span what the connection request can answer: each class of status, and a
    // reply naming none at all. A `2xx` admits the connection; anything else refuses the
    // handshake with that status and the body the function returned.
    let cases: [(&str, &str, Expected); 6] = [
        (
            "a 200, which admits the connection",
            r#"{"statusCode": 200}"#,
            Ok(()),
        ),
        (
            "another success status, which admits it too",
            r#"{"statusCode": 201, "body": "ready"}"#,
            Ok(()),
        ),
        (
            "a 403 with the body the function chose",
            r#"{"statusCode": 403, "body": "denied"}"#,
            Err((403, "denied")),
        ),
        (
            "a 401, which is that status and no other",
            r#"{"statusCode": 401, "body": "who?"}"#,
            Err((401, "who?")),
        ),
        (
            "a 500 the function itself answered",
            r#"{"statusCode": 500, "body": "boom"}"#,
            Err((500, "boom")),
        ),
        (
            "a reply naming no status, which is no decision at all",
            r#"{"ok": true}"#,
            Err((500, "")),
        ),
    ];
    for (row, reply, expected) in cases {
        let case = row;
        let (emulator, engine) = emulator();
        let queue = given_queue(&emulator, case).await;
        let connect = given_function(
            &emulator,
            &engine,
            case,
            "connect-decision",
            &recording_handler("$connect", &queue, reply),
        )
        .await;
        let api = given_api(&emulator, case).await;
        given_route(&emulator, case, &api.api_id, "$connect", &connect, false, None).await;
        given_stage(&emulator, case, &api.api_id).await;

        let answered = handshake_at(case, &api.authority, &format!("/{STAGE}")).await;
        match (expected, answered) {
            (Ok(()), Ok(_socket)) => {}
            (Ok(()), Err(status)) => {
                panic!("case {case:?}: the handshake must be completed, got {status}")
            }
            (Err((expected_status, _body)), Err(status)) => assert_eq!(
                status, expected_status,
                "case {case:?}: expected the handshake refused {expected_status}, got {status}"
            ),
            (Err((expected_status, _body)), Ok(_socket)) => panic!(
                "case {case:?}: expected the handshake refused {expected_status}, got 101"
            ),
        }
        // The function RAN either way: a connection request is served before the handshake
        // is answered, and it is given the connection id the gateway minted for it.
        let connect_event = recorded_event(&emulator, case, &queue, "$connect").await;
        assert!(
            member(&connect_event, "/requestContext/connectionId")
                .as_str()
                .is_some_and(|id| !id.trim().is_empty()),
            "case {case:?}: the connection request must carry a connectionId, got \
             {connect_event}"
        );
    }
}

#[tokio::test]
async fn a_request_authorizer_decides_the_handshake_and_its_context_reaches_the_routes() {
    // The rows span the three answers the authorization owes: the identity source absent
    // (the function not invoked at all), the function denying, and the function allowing —
    // whose context then reaches the message route's own event.
    let cases: [(&str, &str, Option<u16>, bool); 4] = [
        (
            "an authorizer that authorizes, which completes the handshake",
            "?token=open",
            None,
            true,
        ),
        (
            "an authorizer that refuses, which refuses the handshake",
            "?token=shut",
            Some(FORBIDDEN),
            true,
        ),
        (
            "the identity source absent, which refuses it without invoking the function",
            "",
            Some(UNAUTHORIZED),
            false,
        ),
        (
            "the identity source present but empty, which is absent too",
            "?token=",
            Some(UNAUTHORIZED),
            false,
        ),
    ];
    // And each row is driven once per FORMAT its function states the decision in: the IAM
    // policy document a Lambda authorizer must return, and the boolean it "can return …
    // instead of an IAM policy". A WebSocket api's authorizer declares neither
    // `EnableSimpleResponses` nor `AuthorizerPayloadFormatVersion` — both are an HTTP api's —
    // so the answer itself is what says which of the two it is.
    type Handler = fn(&str) -> String;
    let formats: [(&str, Handler, bool); 2] = [
        ("an IAM policy document", authorizer_handler, true),
        ("the boolean of a simple response", simple_authorizer_handler, false),
    ];
    for (format, handler, names_a_principal) in formats {
        for (row, query, refused, invoked) in cases {
            let case = &format!("{row}, stated as {format}");
            let case = case.as_str();
            let (emulator, engine) = emulator();
            let queue = given_queue(&emulator, case).await;
            let authorizer_function = given_function(
                &emulator,
                &engine,
                case,
                "connect-authorizer",
                &handler(&queue),
            )
            .await;
            let connect = given_function(
                &emulator,
                &engine,
                case,
                "connect-route",
                &recording_handler("$connect", &queue, "{\"statusCode\": 200}"),
            )
            .await;
            let ping =
                given_function(&emulator, &engine, case, "ping-route", &echoing_handler("ping"))
                    .await;

            let api = given_api(&emulator, case).await;
            let authorizer_id =
                given_authorizer(&emulator, case, &api.api_id, &authorizer_function).await;
            given_route(
                &emulator,
                case,
                &api.api_id,
                "$connect",
                &connect,
                false,
                Some(&authorizer_id),
            )
            .await;
            given_route(&emulator, case, &api.api_id, "ping", &ping, true, None).await;
            given_stage(&emulator, case, &api.api_id).await;

            // The handshake is offered again while the gateway is still answering its own
            // `500` and the row expects another status: the authorizer's FIRST invocation
            // starts that function's runtime, and on a loaded host that start can overrun the
            // deadline the gateway gives it — a transient refusal the next handshake does not
            // get. The wait is for a decision, and nothing here asserts on the clock.
            let deadline = std::time::Instant::now() + DECISION_WAIT;
            let answered = loop {
                let answered =
                    handshake_at(case, &api.authority, &format!("/{STAGE}{query}")).await;
                let transient = matches!(answered, Err(status)
                    if status == INTERNAL_SERVER_ERROR && refused != Some(INTERNAL_SERVER_ERROR));
                if !transient || std::time::Instant::now() >= deadline {
                    break answered;
                }
                tokio::time::sleep(LOOK_INTERVAL).await;
            };
            let mut socket = match (refused, answered) {
                (None, Ok(socket)) => socket,
                (None, Err(status)) => {
                    panic!("case {case:?}: the handshake must be completed, got {status}")
                }
                (Some(expected), Err(status)) => {
                    assert_eq!(
                        status, expected,
                        "case {case:?}: expected the handshake refused {expected}, got {status}"
                    );
                    if !invoked {
                        // "Otherwise, it returns a 401 Unauthorized response without calling
                        // the Lambda function": nothing of the api ran, so nothing recorded.
                        let records = drained(&emulator, case, &queue).await;
                        assert!(
                            records.is_empty(),
                            "case {case:?}: no function may run for a handshake refused \
                             before the authorizer, got {records:?}"
                        );
                    }
                    continue;
                }
                (Some(expected), Ok(_socket)) => {
                    panic!("case {case:?}: expected the handshake refused {expected}, got 101")
                }
            };

            // The authorizer ran, and it was given the connection request's own event.
            let authorizer_event = recorded_event(&emulator, case, &queue, "authorizer").await;
            for (what, pointer, expected) in [
                ("the authorizer kind", "/type", json!("REQUEST")),
                (
                    "the call it decides",
                    "/requestContext/eventType",
                    json!("CONNECT"),
                ),
                (
                    "the route it decides",
                    "/requestContext/routeKey",
                    json!("$connect"),
                ),
            ] {
                assert_eq!(
                    member(&authorizer_event, pointer),
                    expected,
                    "case {case:?}: expected {what} at {pointer} to be {expected}, got \
                     {authorizer_event}"
                );
            }
            assert!(
                member(&authorizer_event, "/methodArn")
                    .as_str()
                    .is_some_and(|arn| arn.contains(&api.api_id)
                        && arn.contains(STAGE)
                        && arn.ends_with("$connect")),
                "case {case:?}: the authorizer event must name the route it decides, got \
                 {authorizer_event}"
            );

            // And what it ALLOWED reaches every later call of the connection.
            let answer = exchanged_reply(case, &mut socket, r#"{"action":"ping"}"#).await;
            let answered: Value = serde_json::from_str(&answer).unwrap_or_else(|error| {
                panic!("case {case:?}: expected JSON, got {answer:?}: {error}")
            });
            for (what, pointer, expected) in [
                (
                    "the context the authorizer returned",
                    "/event/requestContext/authorizer/tier",
                    json!("gold"),
                ),
                (
                    "every member of it",
                    "/event/requestContext/authorizer/token",
                    json!("open"),
                ),
                (
                    // A policy-document answer names a principal beside its context; the
                    // boolean names none, so nothing is reported for one.
                    "the principal it named",
                    "/event/requestContext/authorizer/principalId",
                    if names_a_principal {
                        json!("connection-principal")
                    } else {
                        Value::Null
                    },
                ),
            ] {
                assert_eq!(
                    member(&answered, pointer),
                    expected,
                    "case {case:?}: expected {what} at {pointer} to be {expected}, got \
                     {answered}"
                );
            }
        }
    }
}

/// What one row of the grant tables below puts on the route's function before a client dials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Nothing at all: the function carries no resource policy.
    None,
    /// `lambda:InvokeFunction` to the gateway, naming no source.
    AnySource,
    /// The same, narrowed to a source ARN the row builds from the api it created.
    Narrowed(Source),
    /// `lambda:InvokeFunction` to ANOTHER service principal.
    OtherPrincipal,
    /// Another action to the gateway.
    OtherAction,
    /// The unnarrowed grant, added and then revoked again.
    Revoked,
}

/// Which `aws:SourceArn` a narrowed grant names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// Every stage and route of THIS api.
    ThisApiWildcard,
    /// This route exactly: this api, this stage, this route key.
    ThisRoute,
    /// Another route of this api.
    AnotherRoute,
    /// Another api altogether.
    AnotherApi,
}

impl Source {
    /// The ARN this source names, for the api `api_id`, the route `route_key` and the account
    /// the emulator serves.
    ///
    /// The form is the `execute-api` resource ARN of a WEBSOCKET route —
    /// `arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{route-key}` — the same value
    /// the connection request's authorizer event names as its `methodArn`, asserted above in
    /// `a_request_authorizer_decides_the_handshake_and_its_context_travels`.
    fn arn(self, api_id: &str, route_key: &str) -> String {
        let account = engine::emulator_account_id();
        let resource = match self {
            Self::ThisApiWildcard => format!("{api_id}/*/*"),
            Self::ThisRoute => format!("{api_id}/{STAGE}/{route_key}"),
            Self::AnotherRoute => format!("{api_id}/{STAGE}/$disconnect"),
            Self::AnotherApi => "otherapi00/*/*".to_string(),
        };
        format!("arn:aws:execute-api:{REGION}:{account}:{resource}")
    }
}

/// Put `grant` on `function` for the route `route_key` of `api_id`.
async fn given_grant(
    emulator: &MultiServiceRouter,
    case: &str,
    function: &str,
    api_id: &str,
    route_key: &str,
    grant: Grant,
) {
    match grant {
        Grant::None => {}
        Grant::AnySource => gateway_may_invoke(emulator, case, function, None).await,
        Grant::Narrowed(source) => {
            let arn = source.arn(api_id, route_key);
            gateway_may_invoke(emulator, case, function, Some(&arn)).await;
        }
        Grant::OtherPrincipal => {
            granted(
                emulator,
                case,
                function,
                "lambda:InvokeFunction",
                "events.amazonaws.com",
                None,
            )
            .await;
        }
        Grant::OtherAction => {
            granted(
                emulator,
                case,
                function,
                "lambda:GetFunction",
                "apigateway.amazonaws.com",
                None,
            )
            .await;
        }
        Grant::Revoked => {
            gateway_may_invoke(emulator, case, function, None).await;
            revoked(
                emulator,
                case,
                function,
                "lambda:InvokeFunction",
                "apigateway.amazonaws.com",
                None,
            )
            .await;
        }
    }
}

/// Take the grant for the gateway's unnarrowed `lambda:InvokeFunction` back off `function`.
async fn revoke_gateway_grant(emulator: &MultiServiceRouter, case: &str, function: &str) {
    revoked(
        emulator,
        case,
        function,
        "lambda:InvokeFunction",
        "apigateway.amazonaws.com",
        None,
    )
    .await;
}

/// One row of the grant tables: what the function carries, and whether the gateway's
/// invocation is admitted.
struct GrantRow {
    case: &'static str,
    grant: Grant,
    admitted: bool,
}

/// The grants every table row below spans: no policy at all, a grant naming no source, a grant
/// narrowed to the api with wildcards, to this route exactly, to ANOTHER route of the same api
/// and to another api altogether, a grant to another service principal, a grant of another
/// action, and a grant that is REVOKED again.
const GRANT_ROWS: &[GrantRow] = &[
    GrantRow {
        // The recorded case: the function the connection request's route fronts carries no
        // grant for the gateway at all.
        case: "a function carrying no resource policy at all",
        grant: Grant::None,
        admitted: false,
    },
    GrantRow {
        case: "a grant to the gateway naming no source",
        grant: Grant::AnySource,
        admitted: true,
    },
    GrantRow {
        case: "a grant narrowed to every stage and route of this api",
        grant: Grant::Narrowed(Source::ThisApiWildcard),
        admitted: true,
    },
    GrantRow {
        case: "a grant narrowed to this route exactly",
        grant: Grant::Narrowed(Source::ThisRoute),
        admitted: true,
    },
    GrantRow {
        case: "a grant narrowed to another route of this api",
        grant: Grant::Narrowed(Source::AnotherRoute),
        admitted: false,
    },
    GrantRow {
        case: "a grant narrowed to another api altogether",
        grant: Grant::Narrowed(Source::AnotherApi),
        admitted: false,
    },
    GrantRow {
        case: "a grant to another service principal",
        grant: Grant::OtherPrincipal,
        admitted: false,
    },
    GrantRow {
        case: "a grant of another action to the gateway",
        grant: Grant::OtherAction,
        admitted: false,
    },
    GrantRow {
        case: "a grant that was added and then revoked",
        grant: Grant::Revoked,
        admitted: false,
    },
];

/// The CONNECTION REQUEST's own function: it is invoked only where its resource policy admits
/// the gateway, and a handshake whose invocation was not made is refused `500` with the
/// deployment package never run.
///
/// Whether the function RAN is read from its own side effect — the record it writes to the
/// queue — so a refused row asserts that nothing ran rather than merely that the client was
/// refused.
#[tokio::test]
async fn a_connect_routes_function_is_invoked_only_where_its_policy_admits_the_gateway() {
    for (index, row) in GRANT_ROWS.iter().enumerate() {
        let case = row.case;
        let (emulator, engine) = emulator();
        let queue = given_queue(&emulator, case).await;
        let function = format!("connect-grant-{index}");
        let arn = given_ungranted_function(
            &emulator,
            &engine,
            case,
            &function,
            &recording_handler("$connect", &queue, "{\"statusCode\": 200}"),
        )
        .await;
        let api = given_api(&emulator, case).await;
        given_route(&emulator, case, &api.api_id, "$connect", &arn, false, None).await;
        given_stage(&emulator, case, &api.api_id).await;
        given_grant(
            &emulator,
            case,
            &function,
            &api.api_id,
            "$connect",
            row.grant,
        )
        .await;

        let opened = handshake_at(case, &api.authority, &format!("/{STAGE}")).await;
        if row.admitted {
            assert!(
                opened.is_ok(),
                "case {case:?}: expected the handshake to complete once the function's policy \
                 admits the gateway, got {:?}",
                opened.as_ref().err()
            );
            // The record proves the deployment package ran for the connection request.
            recorded_event(&emulator, case, &queue, "$connect").await;
            continue;
        }
        let refused = opened.err();
        assert_eq!(
            refused,
            Some(INTERNAL_SERVER_ERROR),
            "case {case:?}: expected {INTERNAL_SERVER_ERROR} for an invocation the function's \
             policy does not admit, got {refused:?}"
        );
        // The connection request's invocation is synchronous — the handshake is answered after
        // the function would have returned — so the queue AT THIS INSTANT says whether the
        // deployment package ran at all.
        let records = drained(&emulator, case, &queue).await;
        assert!(
            records.is_empty(),
            "case {case:?}: expected no invocation of the function at all, got {records:?}"
        );
    }
}

/// The same grant decides a MESSAGE's route: the frame's function is invoked only where its
/// policy admits the gateway, and a message whose invocation was not made reads the gateway's
/// own `Internal server error` frame.
///
/// The api holds no `$connect` route, so the handshake completes whatever the message route's
/// function is granted — which is what leaves the grant as the only difference between rows.
#[tokio::test]
async fn a_message_routes_function_is_invoked_only_where_its_policy_admits_the_gateway() {
    for (index, row) in GRANT_ROWS.iter().enumerate() {
        let case = row.case;
        let (emulator, engine) = emulator();
        let queue = given_queue(&emulator, case).await;
        let function = format!("message-grant-{index}");
        let arn = given_ungranted_function(
            &emulator,
            &engine,
            case,
            &function,
            &recording_handler(
                "sendmessage",
                &queue,
                "{\"statusCode\": 200, \"body\": \"served\"}",
            ),
        )
        .await;
        let api = given_api(&emulator, case).await;
        given_route(
            &emulator,
            case,
            &api.api_id,
            "sendmessage",
            &arn,
            true,
            None,
        )
        .await;
        given_stage(&emulator, case, &api.api_id).await;
        given_grant(
            &emulator,
            case,
            &function,
            &api.api_id,
            "sendmessage",
            row.grant,
        )
        .await;

        let mut socket = handshake_at(case, &api.authority, &format!("/{STAGE}"))
            .await
            .unwrap_or_else(|status| {
                panic!(
                    "case {case:?}: an api with no $connect route completes the handshake, got \
                     {status}"
                )
            });
        let reply = exchanged_reply(case, &mut socket, r#"{"action":"sendmessage"}"#).await;
        if row.admitted {
            assert_eq!(
                reply, "served",
                "case {case:?}: expected the function's own reply, got {reply:?}"
            );
            recorded_event(&emulator, case, &queue, "sendmessage").await;
            continue;
        }
        let frame: Value = serde_json::from_str(&reply).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the gateway's own frame, got {reply:?}: {error}")
        });
        assert_eq!(
            member(&frame, "/message"),
            json!("Internal server error"),
            "case {case:?}: expected the gateway's own sentence, got {frame}"
        );
        let records = drained(&emulator, case, &queue).await;
        assert!(
            records.is_empty(),
            "case {case:?}: expected no invocation of the function at all, got {records:?}"
        );
    }
}

/// The grant decides the NEXT handshake, in both directions: the policy is read from Lambda per
/// invocation rather than held by this service, so adding the permission makes a refused
/// connection request complete and revoking it makes a completing one refuse — with no
/// redeploy of the stage and no change to the api.
#[tokio::test]
async fn a_permission_added_or_revoked_decides_the_next_connection_request() {
    let case = "the grant is added and revoked while the stage serves";
    let (emulator, engine) = emulator();
    let queue = given_queue(&emulator, case).await;
    let function = "connect-grant-lifecycle";
    let arn = given_ungranted_function(
        &emulator,
        &engine,
        case,
        function,
        &recording_handler("$connect", &queue, "{\"statusCode\": 200}"),
    )
    .await;
    let api = given_api(&emulator, case).await;
    given_route(&emulator, case, &api.api_id, "$connect", &arn, false, None).await;
    given_stage(&emulator, case, &api.api_id).await;

    // Before the grant: refused, and nothing ran.
    let refused = handshake_at(case, &api.authority, &format!("/{STAGE}"))
        .await
        .err();
    assert_eq!(
        refused,
        Some(INTERNAL_SERVER_ERROR),
        "case {case:?}: expected {INTERNAL_SERVER_ERROR} before the permission exists, got \
         {refused:?}"
    );
    let records = drained(&emulator, case, &queue).await;
    assert!(
        records.is_empty(),
        "case {case:?}: expected no invocation before the permission exists, got {records:?}"
    );

    // Added: the next handshake is admitted by the function itself.
    gateway_may_invoke(&emulator, case, function, None).await;
    let opened = handshake_at(case, &api.authority, &format!("/{STAGE}")).await;
    assert!(
        opened.is_ok(),
        "case {case:?}: expected the handshake to complete once the permission exists, got {:?}",
        opened.as_ref().err()
    );
    recorded_event(&emulator, case, &queue, "$connect").await;
    drop(opened);

    // Revoked: the next handshake is refused again.
    revoke_gateway_grant(&emulator, case, function).await;
    let refused = handshake_at(case, &api.authority, &format!("/{STAGE}"))
        .await
        .err();
    assert_eq!(
        refused,
        Some(INTERNAL_SERVER_ERROR),
        "case {case:?}: expected {INTERNAL_SERVER_ERROR} once the permission is revoked, got \
         {refused:?}"
    );
}

/// A handler that RAISES: its route's integration ran and produced no reply the gateway could
/// deliver, which is the one call a client reads the gateway's own frame for.
const RAISING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   raise ValueError(\"the backend failed\")\n";

/// The fixed text of that frame. It is the gateway's sentence for a call it could not serve,
/// never an answer for a healthy integration.
const INTERNAL_SERVER_ERROR_FRAME: &str = "Internal server error";

/// What one frame of the established connection must be answered with.
#[derive(Debug, Clone, Copy)]
enum Answered {
    /// The reply of the route carrying this key — the payload ITS OWN function returned.
    By(&'static str),
    /// The gateway's own frame, carrying this sentence and naming the connection and the call.
    GatewayFrame(&'static str),
}

/// One frame a client sends over the established connection, and what must come back.
struct Frame {
    case: &'static str,
    sent: &'static str,
    answered: Answered,
}

/// The rows span what the dispatch rule generalises over: an action each custom route holds, an
/// action no route holds, a frame carrying no value at that path at all (absent, and present as
/// `null`), a value at it that is not a string, a frame that is no JSON object, an action whose
/// route's integration FAILED, and the first action once more over the same connection.
const FRAMES: &[Frame] = &[
    Frame {
        // The recorded case: the action a custom route is bound to.
        case: "an action a custom route holds",
        sent: r#"{"action":"alpha","marker":"mk-alpha"}"#,
        answered: Answered::By("alpha"),
    },
    Frame {
        case: "another action, which is another route's own function",
        sent: r#"{"action":"sendmessage","text":"hello"}"#,
        answered: Answered::By("sendmessage"),
    },
    Frame {
        case: "an action no route of the api holds",
        sent: r#"{"action":"nowhere"}"#,
        answered: Answered::By("$default"),
    },
    Frame {
        case: "a frame carrying no action member at all",
        sent: r#"{"greeting":"hello"}"#,
        answered: Answered::By("$default"),
    },
    Frame {
        case: "a frame whose action is null",
        sent: r#"{"action":null}"#,
        answered: Answered::By("$default"),
    },
    Frame {
        case: "a frame whose action is not a string",
        sent: r#"{"action":{"name":"alpha"}}"#,
        answered: Answered::By("$default"),
    },
    Frame {
        case: "a frame that is no JSON object",
        sent: "plain text, no JSON",
        answered: Answered::By("$default"),
    },
    Frame {
        case: "an action whose route's integration FAILED",
        sent: r#"{"action":"boom"}"#,
        answered: Answered::GatewayFrame(INTERNAL_SERVER_ERROR_FRAME),
    },
    Frame {
        case: "the first action once more, after that failure",
        sent: r#"{"action":"alpha","marker":"mk-alpha-again"}"#,
        answered: Answered::By("alpha"),
    },
];

/// One answered frame, read as the JSON document it is.
fn parsed(case: &str, row: &str, answer: &str) -> Value {
    serde_json::from_str(answer).unwrap_or_else(|error| {
        panic!("case {case:?} / {row}: expected a JSON frame, got {answer:?}: {error}")
    })
}

/// Offer the handshake at the announced socket until it is COMPLETED, or [`DECISION_WAIT`]
/// passes.
///
/// A loaded host can answer the gateway's own `500` for a connection request whose function
/// runtime is still starting ([`INTERNAL_SERVER_ERROR`], the transient [`DECISION_WAIT`] is
/// there for), so the wait is for the UPGRADE — a condition — and nothing here asserts on the
/// clock. Any other refusal fails the case at once.
async fn upgraded(case: &str, authority: &str) -> ClientSocket {
    let deadline = std::time::Instant::now() + DECISION_WAIT;
    loop {
        match handshake_at(case, authority, &format!("/{STAGE}")).await {
            Ok(socket) => return socket,
            Err(status) => {
                assert!(
                    status == INTERNAL_SERVER_ERROR
                        && std::time::Instant::now() < deadline,
                    "case {case:?}: the handshake against the announced socket {authority:?} \
                     must be completed, got {status}"
                );
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
        }
    }
}

/// Send `sent` until a ROUTE answered it, or `patience` passes: the same transient the
/// handshake waits out above reaches a message too, where it arrives as the gateway's own
/// `Internal server error` frame for a route whose function the invocation could not get an
/// answer out of in time. The wait is for the route's own reply — a condition — and a frame
/// that carries anything else fails the case at once; `patience` is ONE budget for the whole
/// table, since every row of it waits for the same thing.
async fn route_reply(
    case: &str,
    row: &str,
    socket: &mut ClientSocket,
    sent: &str,
    patience: std::time::Instant,
) -> Value {
    loop {
        let answered = parsed(case, row, &exchanged_reply(row, socket, sent).await);
        if member(&answered, "/fn") != Value::Null {
            return answered;
        }
        assert!(
            member(&answered, "/message") == json!(INTERNAL_SERVER_ERROR_FRAME)
                && std::time::Instant::now() < patience,
            "case {case:?} / {row}: expected the reply of the route the frame selects, got \
             {answered}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The connection a frame's answer names: the event's own `connectionId` for a route's reply,
/// and the gateway frame's for the gateway's.
fn named_connection(answered: &Value, frame: &Frame) -> Value {
    match frame.answered {
        Answered::By(_) => member(answered, "/event/requestContext/connectionId"),
        Answered::GatewayFrame(_) => member(answered, "/connectionId"),
    }
}

/// The whole chain a WebSocket client drives, against the address the api ANNOUNCED: the
/// handshake completes there, each frame is then dispatched to the route the api's
/// `RouteSelectionExpression` selects — the `$default` route where it selects nothing the api
/// holds — and the client reads back THAT route's integration's own payload. The gateway's
/// `Internal server error` frame is reserved for the one route whose integration failed, and
/// the connection is still served by its healthy routes afterwards.
#[tokio::test]
async fn each_frame_is_answered_by_its_selected_routes_integration_and_only_a_failure_by_the_gateway()
{
    let case = "an api holding two custom routes, a $default route and a failing route, each \
                fronting its own function";
    let (emulator, engine) = emulator();

    // One function per route key, as a client provisions them.
    let connect =
        given_function(&emulator, &engine, case, "connect-route", &echoing_handler("$connect"))
            .await;
    let alpha =
        given_function(&emulator, &engine, case, "alpha-route", &echoing_handler("alpha")).await;
    let sendmessage = given_function(
        &emulator,
        &engine,
        case,
        "sendmessage-route",
        &echoing_handler("sendmessage"),
    )
    .await;
    let fallback =
        given_function(&emulator, &engine, case, "default-route", &echoing_handler("$default"))
            .await;
    let failing =
        given_function(&emulator, &engine, case, "failing-route", RAISING_HANDLER).await;

    let api = given_api(&emulator, case).await;
    for (route_key, function, two_way) in [
        ("$connect", &connect, false),
        ("alpha", &alpha, true),
        ("sendmessage", &sendmessage, true),
        ("$default", &fallback, true),
        ("boom", &failing, true),
    ] {
        given_route(
            &emulator,
            case,
            &api.api_id,
            route_key,
            function,
            two_way,
            None,
        )
        .await;
    }
    given_stage(&emulator, case, &api.api_id).await;

    // The handshake is offered at the address the api announced as its `ApiEndpoint`
    // (`api.authority` is read off that value), and completes `101` on the connection
    // request's own 2xx.
    let mut socket: ClientSocket = upgraded(case, &api.authority).await;

    let mut connection: Option<Value> = None;
    // One budget for the whole table (see [`route_reply`]), so a loaded host cannot stretch the
    // case row by row.
    let patience = std::time::Instant::now() + DECISION_WAIT;
    for frame in FRAMES {
        let row = frame.case;
        let answered = dispatched(case, row, &mut socket, frame.sent, frame.answered, patience)
            .await;

        // Whichever answered it, the call belongs to the ONE connection the handshake minted.
        let named = named_connection(&answered, frame);
        assert!(
            named.as_str().is_some_and(|id| !id.trim().is_empty()),
            "case {case:?} / {row}: the answer must name the connection, got {answered}"
        );
        match &connection {
            None => connection = Some(named),
            Some(first) => assert_eq!(
                &named, first,
                "case {case:?} / {row}: every call of this connection names {first}, got \
                 {answered}"
            ),
        }
    }
}

/// The fixed text of the gateway's own frame for a call it routes NOWHERE: what a client reads
/// when the value the api's expression selected matches no route key of the api and the api
/// declares no `$default` route either.
const FORBIDDEN_FRAME: &str = "Forbidden";

/// A handler that BOTH records the event it was given and answers it, so one route's delivery is
/// readable from the reply its two-way route sends back AND from the record it left behind —
/// which is what a frame that reached NO integration is asserted against.
fn recording_echo_handler(name: &str, queue_url: &str) -> String {
    recording_handler(
        name,
        queue_url,
        &format!(
            "{{\"statusCode\": 200, \"body\": json.dumps({{\"fn\": {name:?}, \"event\": event}})}}"
        ),
    )
}

/// Every record written to `queue_url` so far, accumulated over repeated looks until one
/// carrying each of `awaited` at `pointer` has arrived; nothing awaited is ONE look, which is
/// what a case that must find no record at all reads.
///
/// [`drained`] takes each look's messages OFF the queue, so a case that waits for one record and
/// then asserts another is ABSENT has to keep what it has already seen: this is that
/// accumulation. The wait is for records the calls already made owe, so it bounds no behaviour
/// any case asserts on and nothing here reads the clock as a result.
async fn records_awaiting(
    emulator: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    pointer: &str,
    awaited: &[&str],
) -> Vec<Value> {
    let deadline = std::time::Instant::now() + RECORD_WAIT;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(drained(emulator, case, queue_url).await);
        let missing: Vec<&str> = awaited
            .iter()
            .copied()
            .filter(|wanted| {
                !seen
                    .iter()
                    .any(|record| member(record, pointer) == json!(wanted))
            })
            .collect();
        if missing.is_empty() {
            // One more look once the awaited records have landed: a record the queue already
            // held but did not hand back in the look that satisfied the wait is then part of
            // the set an absence is asserted over, rather than left behind.
            seen.extend(drained(emulator, case, queue_url).await);
            return seen;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected a record carrying each of {awaited:?} at {pointer}, still \
             missing {missing:?} among {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What one row of [`CONNECT_DECISIONS`] presents at the authorizer's identity source, and what
/// the connection request owes for it.
struct ConnectDecision {
    case: &'static str,
    /// The value the handshake's `token` query parameter carries, or `None` when the parameter
    /// is not carried at all. Every row's value is its own, so each invocation a row causes is
    /// told apart from every other row's by the token its event carries.
    token: Option<&'static str>,
    /// The status the handshake is refused with, or `None` when it is completed.
    refused: Option<u16>,
    /// Whether the AUTHORIZER's own function is invoked at all.
    authorizer_runs: bool,
    /// Whether the `$connect` route's INTEGRATION is performed.
    integration_runs: bool,
}

impl ConnectDecision {
    /// The query string a client dials the stage with for this row.
    fn query(&self) -> String {
        match self.token {
            Some(token) => format!("?token={token}"),
            None => String::new(),
        }
    }
}

/// The rows span what the decision generalises over: two different tokens the authorizer denies,
/// the identity source absent — carried empty, and not carried at all — and the token it allows.
///
/// The ORDER matters, and the admitted row is deliberately LAST: its `$connect` record is the
/// evidence every invocation this table causes has landed, since the gateway performs the
/// connection request before answering each handshake, and each handshake here is answered before
/// the next is offered. Asserting the absence of a record then needs no wait on the clock.
const CONNECT_DECISIONS: &[ConnectDecision] = &[
    ConnectDecision {
        // The recorded case: a token the authorizer answers an explicit deny for.
        case: "a token the authorizer denies",
        token: Some("shut"),
        refused: Some(FORBIDDEN),
        authorizer_runs: true,
        integration_runs: false,
    },
    ConnectDecision {
        case: "another token it denies, which is every value but the one it admits",
        token: Some("let-me-in"),
        refused: Some(FORBIDDEN),
        authorizer_runs: true,
        integration_runs: false,
    },
    ConnectDecision {
        case: "the identity source carried empty, which is absent",
        token: Some(""),
        refused: Some(UNAUTHORIZED),
        authorizer_runs: false,
        integration_runs: false,
    },
    ConnectDecision {
        case: "the identity source not carried at all",
        token: None,
        refused: Some(UNAUTHORIZED),
        authorizer_runs: false,
        integration_runs: false,
    },
    ConnectDecision {
        case: "the token the authorizer allows",
        token: Some("open"),
        refused: None,
        authorizer_runs: true,
        integration_runs: true,
    },
];

/// The `REQUEST` authorizer of the `$connect` route decides the handshake BEFORE the route's
/// integration is performed: every row reaches a terminal outcome — the upgrade completed, or
/// refused with a definite status — and the `$connect` integration ran for exactly the row the
/// authorizer admitted.
///
/// Every row is offered against ONE api, so what ran is one set of records: each invocation
/// carries the token of the row that caused it, and the whole set is compared to the set the rows
/// own. A record from a row that must cause none — the `$connect` integration of a refused
/// connection request, the authorizer of a handshake presenting nothing — therefore fails the
/// case wherever it lands, and the admitted row's own record (the last invocation the table
/// causes) is what says the set is complete.
#[tokio::test]
async fn a_connect_authorizers_decision_is_terminal_and_decides_whether_the_integration_runs() {
    let case = "one api whose $connect route is guarded by a REQUEST authorizer over a token";
    let (emulator, engine) = emulator();
    let queue = given_queue(&emulator, case).await;
    let authorizer_function = given_function(
        &emulator,
        &engine,
        case,
        "connect-authorizer",
        &authorizer_handler(&queue),
    )
    .await;
    let connect = given_function(
        &emulator,
        &engine,
        case,
        "connect-route",
        &recording_handler("$connect", &queue, "{\"statusCode\": 200}"),
    )
    .await;

    let api = given_api(&emulator, case).await;
    let authorizer_id = given_authorizer(&emulator, case, &api.api_id, &authorizer_function).await;
    given_route(
        &emulator,
        case,
        &api.api_id,
        "$connect",
        &connect,
        false,
        Some(&authorizer_id),
    )
    .await;
    given_stage(&emulator, case, &api.api_id).await;

    // Each row's own handshake, in the table's order, and the status it must be answered with.
    // One socket per admitted row is kept open until the case ends, as a client's would be.
    let mut _upgraded: Vec<ClientSocket> = Vec::new();
    for row in CONNECT_DECISIONS {
        let label = row.case;
        // The handshake is offered again while the gateway is still answering its own `500` for
        // an authorizer whose runtime is starting, which a loaded host produces transiently and
        // no row expects. The wait is for a DECISION — a condition — and nothing here asserts on
        // the clock.
        let deadline = std::time::Instant::now() + DECISION_WAIT;
        let answered = loop {
            let answered =
                handshake_at(label, &api.authority, &format!("/{STAGE}{}", row.query())).await;
            let transient = matches!(answered, Err(status)
                if status == INTERNAL_SERVER_ERROR && row.refused != Some(INTERNAL_SERVER_ERROR));
            if !transient || std::time::Instant::now() >= deadline {
                break answered;
            }
            tokio::time::sleep(LOOK_INTERVAL).await;
        };
        match (row.refused, answered) {
            (None, Ok(socket)) => _upgraded.push(socket),
            (None, Err(status)) => {
                panic!("case {case:?} / {label}: the handshake must be completed, got {status}")
            }
            (Some(expected), Err(status)) => assert_eq!(
                status, expected,
                "case {case:?} / {label}: expected the handshake refused {expected}, got {status}"
            ),
            (Some(expected), Ok(_socket)) => panic!(
                "case {case:?} / {label}: expected the handshake refused {expected}, got 101"
            ),
        }
    }

    // What RAN, as the set of (function, the token the event it was given carried). The wait is
    // for the LAST invocation the table causes — the admitted row's connection request — after
    // which no invocation of any row is still in flight, because each handshake was answered
    // before the next was offered and the connection request is performed before that answer.
    let records = records_awaiting(&emulator, case, &queue, "/fn", &["$connect"]).await;
    // The token is kept as the JSON text its member carries, so a row that sends no token and a
    // row that sends an empty one are two different entries and both are printable.
    let mut ran: BTreeSet<(String, String)> = BTreeSet::new();
    for record in &records {
        ran.insert((
            member(record, "/fn").as_str().unwrap_or_default().to_string(),
            member(record, "/event/queryStringParameters/token").to_string(),
        ));
    }
    let mut owed: BTreeSet<(String, String)> = BTreeSet::new();
    for row in CONNECT_DECISIONS {
        let token = row
            .token
            .map_or(Value::Null, |token| json!(token))
            .to_string();
        if row.authorizer_runs {
            owed.insert(("authorizer".to_string(), token.clone()));
        }
        if row.integration_runs {
            owed.insert(("$connect".to_string(), token));
        }
    }
    assert_eq!(
        ran, owed,
        "case {case:?}: expected exactly the invocations the rows owe, got the records {records:?}"
    );
}

/// Send one frame and assert the answer it owes: the reply of the ROUTE that must serve it — its
/// own function's payload, the kind of call, the route key and the frame itself — or the
/// GATEWAY's own frame, which carries that sentence, names the connection and the call, and is no
/// route's reply. The answered frame is handed back, for a case that reads more of it.
///
/// The ONE statement of what an answered frame must carry, shared by every table in this suite:
/// the rows differ, what each kind of answer owes does not.
async fn dispatched(
    case: &str,
    row: &str,
    socket: &mut ClientSocket,
    sent: &str,
    answered_by: Answered,
    patience: std::time::Instant,
) -> Value {
    match answered_by {
        Answered::By(route_key) => {
            let answered = route_reply(case, row, socket, sent, patience).await;
            for (what, pointer, expected) in [
                ("the function that served it", "/fn", json!(route_key)),
                (
                    "the kind of call",
                    "/event/requestContext/eventType",
                    json!("MESSAGE"),
                ),
                (
                    "the route it was dispatched to",
                    "/event/requestContext/routeKey",
                    json!(route_key),
                ),
                ("the frame itself", "/event/body", json!(sent)),
            ] {
                assert_eq!(
                    member(&answered, pointer),
                    expected,
                    "case {case:?} / {row}: expected {what} at {pointer} to be {expected}, got \
                     {answered}"
                );
            }
            answered
        }
        Answered::GatewayFrame(sentence) => {
            let answered = parsed(case, row, &exchanged_reply(row, socket, sent).await);
            assert_eq!(
                member(&answered, "/message"),
                json!(sentence),
                "case {case:?} / {row}: expected the gateway's {sentence:?} frame, got {answered}"
            );
            assert_eq!(
                member(&answered, "/fn"),
                Value::Null,
                "case {case:?} / {row}: the gateway's frame is no route's reply, got {answered}"
            );
            for (what, pointer) in [
                ("the connection", "/connectionId"),
                ("the call", "/requestId"),
            ] {
                assert!(
                    member(&answered, pointer)
                        .as_str()
                        .is_some_and(|id| !id.trim().is_empty()),
                    "case {case:?} / {row}: the gateway's frame must name {what} at {pointer}, \
                     got {answered}"
                );
            }
            answered
        }
    }
}

/// The two custom routes every api in the two cases below holds, each fronting its own
/// function. Neither case declares a `$default` route, which is what leaves a frame selecting
/// anything else routed NOWHERE.
const CUSTOM_ROUTES: &[&str] = &["alpha", "beta"];

/// The rows span what the drop generalises over: an action each route holds, an action no route
/// holds, an action that only RESEMBLES one, the selected value under another member, a frame
/// whose selected member is `null`, one that is no JSON object at all, and the first action once
/// more — after every drop, over the same connection.
const UNROUTED_FRAMES: &[Frame] = &[
    Frame {
        case: "an action a custom route holds",
        sent: r#"{"action":"alpha"}"#,
        answered: Answered::By("alpha"),
    },
    Frame {
        case: "another action, which is the other route's own function",
        sent: r#"{"action":"beta","text":"hello"}"#,
        answered: Answered::By("beta"),
    },
    Frame {
        // The recorded case: an action matching no route, with no `$default` declared.
        case: "an action no route holds, and no $default route serves",
        sent: r#"{"action":"nowhere"}"#,
        answered: Answered::GatewayFrame(FORBIDDEN_FRAME),
    },
    Frame {
        case: "an action that only resembles one the api holds",
        sent: r#"{"action":"alpha-2"}"#,
        answered: Answered::GatewayFrame(FORBIDDEN_FRAME),
    },
    Frame {
        case: "a route's own key, carried under another member",
        sent: r#"{"name":"alpha"}"#,
        answered: Answered::GatewayFrame(FORBIDDEN_FRAME),
    },
    Frame {
        case: "a frame whose selected member is null",
        sent: r#"{"action":null}"#,
        answered: Answered::GatewayFrame(FORBIDDEN_FRAME),
    },
    Frame {
        case: "a frame that is no JSON object",
        sent: "plain text, no JSON",
        answered: Answered::GatewayFrame(FORBIDDEN_FRAME),
    },
    Frame {
        case: "the first action once more, after every drop",
        sent: r#"{"action":"alpha","marker":"again"}"#,
        answered: Answered::By("alpha"),
    },
];

/// A frame the api routes NOWHERE — nothing the expression selected matches a route key, and the
/// api declares no `$default` route — reaches no integration at all, is answered by the
/// gateway's own frame, and leaves the connection serving the routes the api does hold.
#[tokio::test]
async fn a_frame_the_api_routes_nowhere_reaches_no_integration_and_keeps_the_connection_serving() {
    let case = "an api holding two custom routes and NO $default route";
    let (emulator, engine) = emulator();
    let queue = given_queue(&emulator, case).await;
    let api = given_api(&emulator, case).await;
    for route_key in CUSTOM_ROUTES {
        let function = given_function(
            &emulator,
            &engine,
            case,
            &format!("{route_key}-route"),
            &recording_echo_handler(route_key, &queue),
        )
        .await;
        given_route(&emulator, case, &api.api_id, route_key, &function, true, None).await;
    }
    given_stage(&emulator, case, &api.api_id).await;

    let mut socket: ClientSocket = upgraded(case, &api.authority).await;
    // One budget for the whole table (see [`route_reply`]), so a loaded host cannot stretch the
    // case row by row.
    let patience = std::time::Instant::now() + DECISION_WAIT;
    for row in UNROUTED_FRAMES {
        dispatched(
            case,
            row.case,
            &mut socket,
            row.sent,
            row.answered,
            patience,
        )
        .await;
    }

    // And what RAN says the same: a record for each frame a route served, and a record for
    // NOTHING else — no integration was handed a frame the api routes nowhere.
    let delivered: Vec<&str> = UNROUTED_FRAMES
        .iter()
        .filter(|row| matches!(row.answered, Answered::By(_)))
        .map(|row| row.sent)
        .collect();
    let records = records_awaiting(&emulator, case, &queue, "/event/body", &delivered).await;
    for record in &records {
        let body = member(record, "/event/body");
        let body = body.as_str().unwrap_or_default();
        assert!(
            delivered.contains(&body),
            "case {case:?}: only a frame the api routes somewhere may reach an integration, got \
             the record {record}"
        );
    }
}

/// One phase of [`a_replaced_route_selection_expression_redirects_later_frames`]: the expression
/// the api carries, and the frames a connection opened while it does sends.
struct SelectionPhase {
    case: &'static str,
    /// The `RouteSelectionExpression` the api carries — the api is created with the first
    /// phase's, and each later phase replaces it.
    expression: &'static str,
    /// The frames this phase's connection sends, as [`Frame`] rows.
    frames: &'static [Frame],
}

/// The phases span what the replacement generalises over: the path's DEPTH (a member of the
/// message, then a nested one), WHICH member at the same depth is read, a frame keyed the way an
/// earlier phase's path named, and a frame carrying both paths at once.
///
/// Every frame below carries its own `sentIn` marker, so no two phases send the same body: the
/// closing check — that a record exists for exactly the frames a route served — can then tell a
/// phase's drop from another phase's delivery.
const SELECTION_PHASES: &[SelectionPhase] = &[
    SelectionPhase {
        case: "the expression the api was created with",
        expression: ACTION_SELECTION_EXPRESSION,
        frames: &[
            Frame {
                case: "the created path selects a route",
                sent: r#"{"action":"beta","sentIn":"created"}"#,
                answered: Answered::By("beta"),
            },
            Frame {
                case: "and the other route under the same path",
                sent: r#"{"action":"alpha","sentIn":"created"}"#,
                answered: Answered::By("alpha"),
            },
        ],
    },
    SelectionPhase {
        // The recorded case: the expression is replaced, and a frame then selects the newly
        // addressed route.
        case: "a nested path, replacing the created one",
        expression: "$request.body.meta.kind",
        frames: &[
            Frame {
                case: "a frame keyed by the new path reaches that route",
                sent: r#"{"meta":{"kind":"alpha"},"sentIn":"nested"}"#,
                answered: Answered::By("alpha"),
            },
            Frame {
                case: "the new path decides, not the member the old one named",
                sent: r#"{"meta":{"kind":"beta"},"action":"alpha","sentIn":"nested"}"#,
                answered: Answered::By("beta"),
            },
            Frame {
                case: "a frame keyed the way the old path named is routed nowhere",
                sent: r#"{"action":"beta","sentIn":"nested"}"#,
                answered: Answered::GatewayFrame(FORBIDDEN_FRAME),
            },
        ],
    },
    SelectionPhase {
        case: "another member at the same depth",
        expression: "$request.body.meta.route",
        frames: &[
            Frame {
                case: "the member the expression names is the one read",
                sent: r#"{"meta":{"route":"alpha","kind":"beta"},"sentIn":"sibling"}"#,
                answered: Answered::By("alpha"),
            },
            Frame {
                case: "the member the previous path named now selects nothing",
                sent: r#"{"meta":{"kind":"beta"},"sentIn":"sibling"}"#,
                answered: Answered::GatewayFrame(FORBIDDEN_FRAME),
            },
        ],
    },
];

/// Replace the api's `RouteSelectionExpression` and release a deployment to its stage: a client
/// changes which path a frame is routed by through `UpdateApi`, and the expression a deployed
/// stage selects by is the api's CURRENT one.
async fn selection_expression_replaced(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    expression: &str,
) {
    let (status, updated) = control(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}"),
        Some(json!({ "routeSelectionExpression": expression })),
    )
    .await;
    // The status a patch of an api answers here is the one `mutating_success_status.rs` owns.
    assert_eq!(
        status, 201,
        "case {case:?}: UpdateApi must answer 201, got {status} {updated}"
    );
    assert_eq!(
        updated["routeSelectionExpression"].as_str(),
        Some(expression),
        "case {case:?}: the api must report the expression it now carries, got {updated}"
    );
    created(
        emulator,
        case,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
    )
    .await;
}

/// A `RouteSelectionExpression` replaced on a deployed api redirects the frames that follow: each
/// phase's connection is routed by the path the api carries then, which is readable in WHICH
/// route's function ran.
#[tokio::test]
async fn a_replaced_route_selection_expression_redirects_later_frames() {
    let case = "an api whose RouteSelectionExpression is replaced, each time redeployed";
    let (emulator, engine) = emulator();
    let queue = given_queue(&emulator, case).await;
    let created_with = SELECTION_PHASES
        .first()
        .expect("the phases name the expression the api is created with")
        .expression;
    let api = given_selecting_api(&emulator, case, created_with).await;
    for route_key in CUSTOM_ROUTES {
        let function = given_function(
            &emulator,
            &engine,
            case,
            &format!("{route_key}-route"),
            &recording_echo_handler(route_key, &queue),
        )
        .await;
        given_route(&emulator, case, &api.api_id, route_key, &function, true, None).await;
    }
    given_stage(&emulator, case, &api.api_id).await;

    let patience = std::time::Instant::now() + DECISION_WAIT;
    let mut delivered: Vec<&str> = Vec::new();
    for (index, phase) in SELECTION_PHASES.iter().enumerate() {
        if index > 0 {
            selection_expression_replaced(&emulator, phase.case, &api.api_id, phase.expression)
                .await;
        }
        // Each phase dials its OWN connection: what a frame selects on is the expression the
        // stage carries when the connection is opened.
        let mut socket: ClientSocket = upgraded(phase.case, &api.authority).await;
        for row in phase.frames {
            let label = &format!("{} / {}", phase.case, row.case);
            dispatched(case, label, &mut socket, row.sent, row.answered, patience).await;
            if matches!(row.answered, Answered::By(_)) {
                delivered.push(row.sent);
            }
        }
    }

    // And no frame any phase's path routed nowhere reached an integration.
    let records = records_awaiting(&emulator, case, &queue, "/event/body", &delivered).await;
    for record in &records {
        let body = member(record, "/event/body");
        let body = body.as_str().unwrap_or_default();
        assert!(
            delivered.contains(&body),
            "case {case:?}: only a frame a phase's path routes somewhere may reach an \
             integration, got the record {record}"
        );
    }
}
