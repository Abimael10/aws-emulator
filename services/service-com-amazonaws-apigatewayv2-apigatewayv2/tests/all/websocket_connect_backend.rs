// SPDX-License-Identifier: CC0-1.0

//! The DATA MAPPING a WEBSOCKET route's integration applies to the call it fronts, the one
//! route of a WEBSOCKET api that may DECLARE request parameters at all, and WHAT DECIDES the
//! connection request the `$connect` route serves.
//!
//! The rule the first two tables below are slices of: **a WEBSOCKET route's integration passes
//! the call to its backend as its `RequestParameters` map it — each
//! `integration.request.{location}.{name}` key carrying either a request parameter of the
//! call, written `route.request.{location}.{name}` or `method.request.{location}.{name}`, or
//! a static value "enclosed within single quotes" — so the function reads the destination
//! name the integration chose, while an entry whose source the call never carried maps
//! nothing at all; and a ROUTE may DECLARE `Route$RequestParameters` only on `$connect`, the
//! route that serves the connection request, every other route of such an api being refused
//! `400` `BadRequestException`.**
//!
//! The rule the LAST table is a slice of: **a WEBSOCKET connection request is admitted or
//! refused by the REPLY the `$connect` route's integration produced, so a route that fronts a
//! BACKEND is refused only by the status that backend answered — and with the body it
//! answered — while a route bound to a `MOCK` is admitted or refused by the reply that
//! loopback integration computes from its own records; the gateway's own `500`
//! `{"message":"Internal server error"}` is owed exactly to a `$connect` route that can serve
//! the connection request no reply at all: one targeting no integration of the api, and a
//! `MOCK` that selects no integration response to answer from. An api with no `$connect` route
//! has nothing to refuse the connection.** `MOCK` is "for integrating the route or method
//! request with API Gateway as a 'loopback' endpoint without invoking any backend. Supported
//! only for WebSocket APIs" and `AWS_PROXY` is
//! "for integrating the route or method request with a Lambda function or other AWS service
//! action" (`smithy_query(query="shape", shape="Integration")`, member `IntegrationType`),
//! which is why the two REACH a connection request's decision differently.
//!
//! Nothing in the first table is a value this emulator fills in: the mapping is asserted
//! through the DECISION of a function that ran, which is why AWS Lambda is mounted beside
//! this service on one engine — the `$connect` handler admits the connection only when the
//! event it was handed carries what the mapping was supposed to put there, so a completed
//! handshake IS the assertion and a refused one is its negation.
//!
//! The rest of the WebSocket lifecycle — the event each call is stated in, the routes a frame
//! is dispatched to, the `$disconnect` call and the `REQUEST` authorizer in front of
//! `$connect` — is `websocket_lambda_routes.rs`'s, and the handshake's own statuses over the
//! api's announced socket are `websocket_transport_data_plane.rs`'s; neither is restated here.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call_in_region, emulator, error_code};
use crate::http_api_backend_dispatch::{emulator_with_lambda, given_function};
use crate::websocket_handshake_data_plane::{message, send, Answer, CLIENT_KEY};
use crate::websocket_transport_data_plane::{dial_loopback, ClientSocket};

/// The Region every request below is signed for — the one the Lambda harness creates its
/// functions in, so the api and the function it names live in the same Region.
const REGION: &str = "us-east-1";

/// The stage every api below deploys.
const STAGE: &str = "prod";

/// The header a case's client sends on the handshake, and its value.
const SOURCE_HEADER: &str = "X-Client-Trace";
const SOURCE_VALUE: &str = "trace-7";

/// The query-string parameter a case's client sends, and its value.
const SOURCE_QUERY: &str = "token";
const SOURCE_QUERY_VALUE: &str = "t-9";

/// The status a completed handshake carries (RFC 6455 §4.1), and the one a gateway answers
/// for a connection request whose function refused it.
const SWITCHING_PROTOCOLS: u16 = 101;
const FORBIDDEN: u16 = 403;

/// The sentence a route that may not declare request parameters is refused with. Fixed text:
/// it names nothing of the request.
const CONNECT_ONLY_MESSAGE: &str =
    "Request parameters are only supported for the $connect route in WEBSOCKET APIs";

/// A WebSocket api whose messages select a route by their `action` member.
fn websocket_api(name: &str) -> Value {
    json!({
        "name": name,
        "protocolType": "WEBSOCKET",
        "routeSelectionExpression": "$request.body.action"
    })
}

/// One control-plane request against this service, signed for [`REGION`].
async fn control(
    emulator: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_in_region(emulator, REGION, method, uri, body).await
}

/// Create one resource under an api and answer the created body, asserting the create was
/// accepted.
async fn created(
    emulator: &MultiServiceRouter,
    case: &str,
    uri: &str,
    body: Value,
    what: &str,
) -> Value {
    let (status, created) = control(emulator, "POST", uri, Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {what} must answer 201, got {status} {created}"
    );
    created
}

/// A `$connect` handler that admits the connection only when `condition` holds of the event
/// it was handed, and refuses it with a `403` otherwise.
///
/// A condition that cannot even be evaluated — a member the event does not carry — raises,
/// which Lambda reports as a function error and the gateway answers as a connection request
/// that produced no reply: a handshake that does not complete either way, which is what
/// every negative row below asserts.
fn deciding_handler(condition: &str) -> String {
    format!(
        "def lambda_handler(event, context):\n\
         \x20   return {{'statusCode': 200 if ({condition}) else 403}}\n"
    )
}

/// Open an RFC 6455 handshake against the api's stage, carrying [`SOURCE_HEADER`] and
/// [`SOURCE_QUERY`]: the established socket, or the status a refusal carried.
///
/// The RFC 6455 §4.1 fields are written here rather than left to the client library, because
/// a request built field by field — the only way to carry the header a case maps — is sent
/// as written.
async fn handshake(case: &str, authority: &str) -> Result<ClientSocket, u16> {
    let stream = dial_loopback(case, authority).await.unwrap_or_else(|error| {
        panic!("case {case:?}: the announced socket {authority:?} must accept a connection, got {error}")
    });
    let request = http::Request::builder()
        .uri(format!(
            "ws://{authority}/{STAGE}?{SOURCE_QUERY}={SOURCE_QUERY_VALUE}"
        ))
        .header("host", authority)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", CLIENT_KEY)
        .header(SOURCE_HEADER, SOURCE_VALUE)
        .body(())
        .expect("the handshake request builds");
    match tokio_tungstenite::client_async(request, stream).await {
        Ok((socket, response)) => {
            assert_eq!(
                response.status().as_u16(),
                SWITCHING_PROTOCOLS,
                "case {case:?}: a completed handshake must answer 101"
            );
            Ok(socket)
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(response.status().as_u16())
        }
        Err(error) => panic!("case {case:?}: the handshake failed before any answer: {error}"),
    }
}

#[tokio::test]
async fn an_integrations_request_parameters_map_onto_what_the_connect_function_reads() {
    // Rows differ along WHAT the entry maps and WHERE: each destination location, each
    // spelling and each form a source takes, a destination colliding with a field the client
    // sent, and — the negative rows — a source the handshake never carried, a destination
    // this data plane does not read, and a function that refuses outright.
    let cases: &[(&str, Value, &str, bool)] = &[
        (
            "a header destination carrying the client's header",
            json!({ "integration.request.header.X-Forwarded-Trace": "route.request.header.X-Client-Trace" }),
            "event['headers']['X-Forwarded-Trace'] == 'trace-7'",
            true,
        ),
        (
            "the same source written as the model documents it",
            json!({ "integration.request.header.X-Forwarded-Trace": "method.request.header.x-client-trace" }),
            "event['headers']['X-Forwarded-Trace'] == 'trace-7'",
            true,
        ),
        (
            "a header destination carrying the handshake's query parameter",
            json!({ "integration.request.header.X-Token": "route.request.querystring.token" }),
            "event['headers']['X-Token'] == 't-9'",
            true,
        ),
        (
            "a header destination carrying a static value",
            json!({ "integration.request.header.X-Fixed": "'fixed-value'" }),
            "event['headers']['X-Fixed'] == 'fixed-value'",
            true,
        ),
        (
            "a query-string destination carrying the client's header",
            json!({ "integration.request.querystring.trace": "route.request.header.X-Client-Trace" }),
            "event['queryStringParameters']['trace'] == 'trace-7'",
            true,
        ),
        (
            "a mapped header is stated in the multi-valued map as well",
            json!({ "integration.request.header.X-Forwarded-Trace": "'one'" }),
            "event['multiValueHeaders']['X-Forwarded-Trace'] == ['one']",
            true,
        ),
        (
            "a destination naming a field the client sent replaces that value",
            json!({ "integration.request.header.X-Client-Trace": "'replaced'" }),
            "event['headers']['X-Client-Trace'] == 'replaced'",
            true,
        ),
        (
            "the handshake's own fields travel on beside the mapped ones",
            json!({ "integration.request.header.X-Fixed": "'fixed-value'" }),
            // Under the name the CLIENT sent: the client library writes its fields
            // lowercased, and the event reports the spelling it was sent with.
            "event['headers']['x-client-trace'] == 'trace-7' and \
             event['queryStringParameters']['token'] == 't-9'",
            true,
        ),
        (
            "an integration that maps nothing passes the call through",
            json!({}),
            "event['headers']['x-client-trace'] == 'trace-7'",
            true,
        ),
        // The negative rows: an entry that maps nothing must leave the destination OFF, and
        // a function that refuses must fail the handshake — so the rows above are not
        // passing for want of any decision at all.
        (
            "a source the handshake never carried maps nothing",
            json!({ "integration.request.header.X-Absent": "route.request.header.X-Never-Sent" }),
            "'X-Absent' in event['headers']",
            false,
        ),
        (
            "a key naming no integration request parameter maps nothing",
            json!({ "x-not-a-destination": "'v'" }),
            "'x-not-a-destination' in event['headers']",
            false,
        ),
        (
            "a function that answers a non-2xx status fails the handshake",
            json!({ "integration.request.header.X-Fixed": "'fixed-value'" }),
            "False",
            false,
        ),
    ];
    for (case, request_parameters, condition, admitted) in cases {
        let (emulator, engine) = emulator_with_lambda();
        let api = created(
            &emulator,
            case,
            "/v2/apis",
            websocket_api("mapped-connect"),
            "CreateApi",
        )
        .await;
        let api_id = as_str(&api, "apiId");
        let announced = as_str(&api, "apiEndpoint");
        let authority = announced
            .strip_prefix("wss://")
            .unwrap_or_else(|| {
                panic!("case {case:?}: a WEBSOCKET api announces a wss:// socket, got {announced:?}")
            })
            .to_string();
        let function = given_function(
            &emulator,
            &engine,
            case,
            "connect-handler",
            &deciding_handler(condition),
        )
        .await;
        let integration = created(
            &emulator,
            case,
            &format!("/v2/apis/{api_id}/integrations"),
            json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": function,
                "requestParameters": request_parameters
            }),
            "CreateIntegration",
        )
        .await;
        created(
            &emulator,
            case,
            &format!("/v2/apis/{api_id}/routes"),
            json!({
                "routeKey": "$connect",
                "target": format!("integrations/{}", as_str(&integration, "integrationId")),
                "requestParameters": {
                    format!("route.request.header.{SOURCE_HEADER}"): { "required": true }
                }
            }),
            "CreateRoute",
        )
        .await;
        created(
            &emulator,
            case,
            &format!("/v2/apis/{api_id}/stages"),
            json!({ "stageName": STAGE }),
            "CreateStage",
        )
        .await;
        created(
            &emulator,
            case,
            &format!("/v2/apis/{api_id}/deployments"),
            json!({ "stageName": STAGE }),
            "CreateDeployment",
        )
        .await;

        let opened = handshake(case, &authority).await;
        match (admitted, &opened) {
            (true, Ok(_)) => {}
            (false, Err(status)) => assert_eq!(
                *status, FORBIDDEN,
                "case {case:?}: expected the status the function answered ({FORBIDDEN}), got \
                 {status}"
            ),
            (true, Err(status)) => panic!(
                "case {case:?}: expected the handshake to complete, got {status}: the \
                 condition {condition:?} did not hold of the $connect event"
            ),
            (false, Ok(_)) => panic!(
                "case {case:?}: expected the handshake to be refused, got \
                 {SWITCHING_PROTOCOLS}: the condition {condition:?} held of the $connect \
                 event"
            ),
        }
    }
}

#[tokio::test]
async fn request_parameters_are_declared_only_on_the_connection_requests_route() {
    let emulator = emulator();
    let declared = json!({ "route.request.header.x-client-trace": { "required": true } });
    // Rows differ along the route key the declaration arrives on: the connection request's,
    // each other reserved key, a key of the api's own naming, and a route declaring an
    // empty map (which declares nothing and so is accepted anywhere).
    let cases: &[(&str, &str, Value, bool)] = &[
        (
            "the connection request's route",
            "$connect",
            declared.clone(),
            true,
        ),
        ("the close's route", "$disconnect", declared.clone(), false),
        ("the catch-all route", "$default", declared.clone(), false),
        (
            "a route of the api's own naming",
            "sendmessage",
            declared.clone(),
            false,
        ),
        (
            "a route declaring an empty map",
            "broadcast",
            json!({}),
            true,
        ),
    ];
    for (case, route_key, parameters, accepted) in cases {
        let api = created(
            &emulator,
            case,
            "/v2/apis",
            websocket_api(&format!("declaring-{}", route_key.replace('$', ""))),
            "CreateApi",
        )
        .await;
        let api_id = as_str(&api, "apiId");
        let (status, body) = control(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({ "routeKey": route_key, "requestParameters": parameters })),
        )
        .await;
        if *accepted {
            assert_eq!(
                status, 201,
                "case {case:?}: expected 201 from CreateRoute, got {status} {body}"
            );
            // Read it back through GetRoute: what the create reported is what a later
            // caller reads.
            let route_id = as_str(&body, "routeId");
            let (status, fetched) = control(
                &emulator,
                "GET",
                &format!("/v2/apis/{api_id}/routes/{route_id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from GetRoute, got {status} {fetched}"
            );
            assert_eq!(
                fetched.get("requestParameters"),
                parameters
                    .as_object()
                    .filter(|declared| !declared.is_empty())
                    .map(|_| parameters),
                "case {case:?}: expected GetRoute to report the declaration, got {fetched}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateRoute, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            CONNECT_ONLY_MESSAGE,
            "case {case:?}: expected the refusal's own sentence, got {body}"
        );
        // Nothing was written: the api still holds no route.
        let (status, routes) =
            control(&emulator, "GET", &format!("/v2/apis/{api_id}/routes"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetRoutes, got {status} {routes}"
        );
        assert_eq!(
            routes.get("items").and_then(Value::as_array).map(Vec::len),
            Some(0),
            "case {case:?}: expected the refused create to have written nothing, got {routes}"
        );
    }
}

#[tokio::test]
async fn a_patch_that_leaves_request_parameters_off_the_connection_requests_route_is_refused() {
    let emulator = emulator();
    let declared = json!({ "route.request.header.x-client-trace": { "required": true } });
    let api = created(
        &emulator,
        "patching a declaration",
        "/v2/apis",
        websocket_api("patch-declaring"),
        "CreateApi",
    )
    .await;
    let api_id = as_str(&api, "apiId");
    let connect = created(
        &emulator,
        "the connection request's route",
        &format!("/v2/apis/{api_id}/routes"),
        json!({ "routeKey": "$connect", "requestParameters": declared.clone() }),
        "CreateRoute",
    )
    .await;
    let other = created(
        &emulator,
        "another route of the api",
        &format!("/v2/apis/{api_id}/routes"),
        json!({ "routeKey": "sendmessage" }),
        "CreateRoute",
    )
    .await;

    // Rows differ along HOW the merged route would end up declaring parameters off
    // `$connect`: adding them to another route, and moving the declaring route onto another
    // key; and — the accepted rows — patching the declaring route's own map, and giving the
    // declaration up in the same patch that moves the key.
    let cases: &[(&str, &str, Value, bool)] = &[
        (
            "parameters added to another route",
            &as_str(&other, "routeId"),
            json!({ "requestParameters": declared.clone() }),
            false,
        ),
        (
            "the declaring route moved onto another key",
            &as_str(&connect, "routeId"),
            json!({ "routeKey": "broadcast" }),
            false,
        ),
        (
            "the declaring route's own map replaced",
            &as_str(&connect, "routeId"),
            json!({ "requestParameters": { "route.request.querystring.token": { "required": false } } }),
            true,
        ),
        (
            "the declaration given up in the patch that moves the key",
            &as_str(&connect, "routeId"),
            json!({ "routeKey": "relay", "requestParameters": {} }),
            true,
        ),
    ];
    for (case, route_id, patch, accepted) in cases {
        let (status, body) = control(
            &emulator,
            "PATCH",
            &format!("/v2/apis/{api_id}/routes/{route_id}"),
            Some(patch.clone()),
        )
        .await;
        if *accepted {
            assert_eq!(
                status, 201,
                "case {case:?}: expected 201 from UpdateRoute, got {status} {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from UpdateRoute, got {status} {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            CONNECT_ONLY_MESSAGE,
            "case {case:?}: expected the refusal's own sentence, got {body}"
        );
    }
}

/// The status and sentence a gateway answers for a connection request its api's own
/// configuration can serve no reply for.
const UNPLACEABLE: u16 = 500;
const UNPLACEABLE_MESSAGE: &str = "Internal server error";

/// The status an ADMITTED connection request earns at this emulator's own SERVICE endpoint,
/// and the error code it carries: that endpoint serves one BUFFERED request per call and so
/// switches no connection to the WebSocket protocol — the api's own announced socket is where
/// the `101` is answered (`websocket_transport_data_plane.rs`, and the first table above).
/// What the last table asserts is that an admitted connection request reaches THIS refusal
/// instead of the [`UNPLACEABLE`] a configuration that cannot be served earns.
const NOT_IMPLEMENTED: u16 = 501;
const NOT_IMPLEMENTED_CODE: &str = "NotImplemented";

/// What a row's `$connect` route fronts.
#[derive(Debug, Clone, Copy)]
enum Connect {
    /// No `$connect` route at all.
    Absent,
    /// A `MOCK` integration carrying the documented request template, which renders the
    /// `2xx` a loopback integration answers from its own records, and the `$default`
    /// integration response that status selects.
    Mock,
    /// The same `MOCK` request template with NO integration response for the status it
    /// renders to select: a loopback integration that answers the connection request nothing
    /// at all.
    MockWithoutResponse,
    /// An `AWS_PROXY` integration whose function answers this `statusCode` and this `body`
    /// (an empty body being a reply that carries none).
    Proxy(u16, &'static str),
}

/// What the handshake to the deployed stage is answered with.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// ADMITTED: nothing of the api refuses the connection request, so the handshake reaches
    /// the capability refusal the service endpoint ends in.
    Admitted,
    /// Refused with the gateway's own `500` and its sentence.
    Unplaceable,
    /// Refused with the status the `$connect` function answered and the body it answered.
    FromTheFunction(u16, &'static str),
}

/// One row of the connection-request table: what the `$connect` route fronts, and what a
/// handshake to the deployed stage is then answered with.
struct AdmissionRow {
    case: &'static str,
    connect: Connect,
    expected: Expected,
}

/// The rows differ along what the `$connect` route fronts — nothing at all, a `MOCK` loopback
/// whose template renders a `2xx`, a `MOCK` with no integration response to select, a function
/// that admits, a function that refuses — and
/// along the status and body that function answers: a `2xx`, a `4xx` the client is refused
/// with, and a `5xx`, each of which the gateway relays rather than replacing with one of its
/// own. The recorded case is the `MOCK` row and the `(200, "")` row, which is the same api's
/// `$connect` route re-pointed onto a Lambda.
///
/// The other configuration the rule refuses — a `$connect` route that targets no integration
/// at all — is not a row, because no caller can deploy one: `CreateDeployment` refuses it with
/// `crate::state::route_key::undefined_route_target_message`, which `route_key_grammar.rs`
/// holds.
const ADMISSION_ROWS: &[AdmissionRow] = &[
    AdmissionRow {
        case: "no $connect route at all: nothing refuses the connection",
        connect: Connect::Absent,
        expected: Expected::Admitted,
    },
    AdmissionRow {
        case: "a $connect route bound to a MOCK loopback that renders a 2xx",
        connect: Connect::Mock,
        expected: Expected::Admitted,
    },
    AdmissionRow {
        case: "a $connect route bound to a MOCK loopback that selects no integration \
               response",
        connect: Connect::MockWithoutResponse,
        expected: Expected::Unplaceable,
    },
    AdmissionRow {
        case: "a $connect route whose function admits the client",
        connect: Connect::Proxy(200, ""),
        expected: Expected::Admitted,
    },
    AdmissionRow {
        case: "a $connect route whose function admits the client with a body",
        connect: Connect::Proxy(201, "welcome"),
        expected: Expected::Admitted,
    },
    AdmissionRow {
        case: "a $connect route whose function refuses the client",
        connect: Connect::Proxy(403, "denied"),
        expected: Expected::FromTheFunction(403, "denied"),
    },
    AdmissionRow {
        case: "a $connect route whose function answers an unauthorized status",
        connect: Connect::Proxy(401, ""),
        expected: Expected::FromTheFunction(401, ""),
    },
    AdmissionRow {
        case: "a $connect route whose function answers a status of its own upstream",
        connect: Connect::Proxy(502, "upstream"),
        expected: Expected::FromTheFunction(502, "upstream"),
    },
];

/// A `$connect` handler that answers one proxy reply, whatever event it is handed.
fn answering_handler(status: u16, body: &str) -> String {
    let reply = if body.is_empty() {
        format!("{{'statusCode': {status}}}")
    } else {
        format!("{{'statusCode': {status}, 'body': '{body}'}}")
    };
    format!("def lambda_handler(event, context):\n    return {reply}\n")
}

/// An api created, routed and deployed to one stage, with the two addresses a case needs.
struct DeployedConnect {
    /// The api's own `execute-api` host — the addressing `Api$ApiEndpoint` documents, and the
    /// one this emulator's front door claims.
    endpoint_host: String,
    /// The `ApiEndpoint` the api reported: the socket its data plane is really served on,
    /// which the capability refusal names.
    announced: String,
}

/// Create a WEBSOCKET api whose `$connect` route fronts `connect`, release a deployment to one
/// stage, and answer how a handshake addresses it.
async fn deploy_connect(
    emulator: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    connect: Connect,
) -> DeployedConnect {
    let api = created(
        emulator,
        case,
        "/v2/apis",
        websocket_api("connect-admission"),
        "CreateApi",
    )
    .await;
    let api_id = as_str(&api, "apiId");
    let announced = as_str(&api, "apiEndpoint");
    // `Some(id)` is a `$connect` route targeting that integration; `None` is no such route at
    // all.
    let target: Option<String> = match connect {
        Connect::Absent => None,
        Connect::Mock | Connect::MockWithoutResponse => {
            let integration = created(
                emulator,
                case,
                &format!("/v2/apis/{api_id}/integrations"),
                json!({
                    "integrationType": "MOCK",
                    "requestTemplates": { "$default": "{\"statusCode\": 200}" }
                }),
                "CreateIntegration",
            )
            .await;
            let integration_id = as_str(&integration, "integrationId");
            if matches!(connect, Connect::Mock) {
                created(
                    emulator,
                    case,
                    &format!(
                        "/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses"
                    ),
                    json!({ "integrationResponseKey": "$default" }),
                    "CreateIntegrationResponse",
                )
                .await;
            }
            Some(integration_id)
        }
        Connect::Proxy(status, body) => {
            let function = given_function(
                emulator,
                engine,
                case,
                &format!("connect-answering-{status}"),
                &answering_handler(status, body),
            )
            .await;
            let integration = created(
                emulator,
                case,
                &format!("/v2/apis/{api_id}/integrations"),
                json!({ "integrationType": "AWS_PROXY", "integrationUri": function }),
                "CreateIntegration",
            )
            .await;
            Some(as_str(&integration, "integrationId"))
        }
    };
    if let Some(integration_id) = target {
        created(
            emulator,
            case,
            &format!("/v2/apis/{api_id}/routes"),
            json!({
                "routeKey": "$connect",
                "target": format!("integrations/{integration_id}")
            }),
            "CreateRoute",
        )
        .await;
    }
    created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": STAGE }),
        "CreateStage",
    )
    .await;
    created(
        emulator,
        case,
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
        "CreateDeployment",
    )
    .await;
    // What the stage serves is the configuration its deployment froze, so the release is read
    // back off the stage rather than assumed.
    let (status, stage) = control(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/stages/{STAGE}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetStage must answer 200, got {status} {stage}"
    );
    assert!(
        !as_str(&stage, "deploymentId").is_empty(),
        "case {case:?}: the stage must carry the released deployment, got {stage}"
    );
    DeployedConnect {
        endpoint_host: format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        announced,
    }
}

/// Send the RFC 6455 opening handshake to the deployed stage at the api's own `execute-api`
/// host, with `fields` added to the three fields of §4.1 — the browser-set ones a seam is
/// reached through among them.
async fn endpoint_handshake(
    emulator: &MultiServiceRouter,
    deployed: &DeployedConnect,
    fields: &[(&str, &str)],
) -> Answer {
    let mut sent: Vec<(&str, &str)> = vec![
        ("connection", "Upgrade"),
        ("upgrade", "websocket"),
        ("sec-websocket-key", CLIENT_KEY),
        ("sec-websocket-version", "13"),
    ];
    sent.extend_from_slice(fields);
    send(
        emulator,
        &deployed.endpoint_host,
        "GET",
        &format!("/{STAGE}"),
        &sent,
    )
    .await
}

/// Assert `answer` is what `expected` says, naming the seam the handshake was answered from.
fn assert_connection_answer(
    case: &str,
    seam: &str,
    deployed: &DeployedConnect,
    expected: Expected,
    answer: &Answer,
) {
    assert_ne!(
        answer.status, SWITCHING_PROTOCOLS,
        "case {case:?} ({seam}): a 101 would promise a socket the service endpoint cannot \
         carry, got {}",
        answer.summary()
    );
    match expected {
        Expected::Admitted => {
            assert_eq!(
                answer.status, NOT_IMPLEMENTED,
                "case {case:?} ({seam}): nothing of this api refuses the connection request, \
                 so the handshake must reach the capability refusal and not a {UNPLACEABLE}, \
                 got {}",
                answer.summary()
            );
            assert_eq!(
                answer.error_type.as_deref(),
                Some(NOT_IMPLEMENTED_CODE),
                "case {case:?} ({seam}): expected the capability refusal's own code, got {}",
                answer.summary()
            );
            assert!(
                message(answer).contains(&deployed.announced),
                "case {case:?} ({seam}): the refusal must name the address this api IS reached \
                 at ({:?}), got {}",
                deployed.announced,
                answer.summary()
            );
        }
        Expected::Unplaceable => {
            assert_eq!(
                answer.status, UNPLACEABLE,
                "case {case:?} ({seam}): a $connect route that can serve the connection \
                 request no reply at all is the gateway's own {UNPLACEABLE}, got {}",
                answer.summary()
            );
            assert_eq!(
                message(answer),
                UNPLACEABLE_MESSAGE,
                "case {case:?} ({seam}): expected the gateway's own sentence, got {}",
                answer.summary()
            );
        }
        Expected::FromTheFunction(status, body) => {
            assert_eq!(
                answer.status, status,
                "case {case:?} ({seam}): expected the status the $connect function answered, \
                 got {}",
                answer.summary()
            );
            assert_eq!(
                answer.text, body,
                "case {case:?} ({seam}): expected the body the $connect function answered, \
                 got {:?}",
                answer.text
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_request_is_answered_by_its_integrations_reply_and_only_a_route_with_none_is_the_gateways_500(
) {
    // The two seams this emulator's own service endpoint answers a handshake from. The second
    // is reached because the front door reads those browser-set fields as cross-site and so
    // names the request no operation of this service, leaving it to the routing-error seam —
    // which carries no transport of its own and performs the connection request over the one
    // this service remembered. The decision is the api's and its backend's, so both seams must
    // answer one status.
    let seams: &[(&str, &[(&str, &str)])] = &[
        ("the handshake the router names an operation", &[]),
        (
            "the handshake the front door reads as cross-site",
            &[
                ("origin", "https://another.example"),
                ("sec-fetch-site", "cross-site"),
            ],
        ),
    ];
    for row in ADMISSION_ROWS {
        let (emulator, engine) = emulator_with_lambda();
        let deployed = deploy_connect(&emulator, &engine, row.case, row.connect).await;
        for (seam, fields) in seams {
            let answer = endpoint_handshake(&emulator, &deployed, fields).await;
            assert_connection_answer(row.case, seam, &deployed, row.expected, &answer);
        }
    }
}
