// SPDX-License-Identifier: CC0-1.0

//! The PATH a Lambda proxy event reports for an invoke of a deployed HTTP api stage.
//!
//! The rule every row below holds the data plane to: **the path a proxy event reports —
//! `rawPath` and `requestContext.http.path` under payload format `2.0`, `path` and
//! `requestContext.path` under `1.0` — is the path the api's OWN endpoint was addressed at:
//! the stage's name as the first segment for a NAMED stage, because that stage's invoke URL
//! carries it ("the stage name is typically appended to this URI to form a complete path to
//! a deployed API stage", `Api$ApiEndpoint`), and no stage segment at all for the `$default`
//! stage, which serves the api's root — whichever of the addressings a client on this
//! machine reached the emulator by.**
//!
//! A client cannot resolve `{api-id}.execute-api.{region}.amazonaws.com` on the machine this
//! emulator runs on, so it carries in the PATH what it cannot carry in the host: the api id
//! ahead of the stage, the local endpoint marker, or the root stage's own name — and none of
//! those is part of the path the api was addressed at. An event that reported one would hand
//! the function a path no AWS deployment produces, so a handler that dispatches on it, or
//! rebuilds a URL or a signature from it, would route differently here than at AWS.
//!
//! The rows differ along what the rule generalises over: which stage serves the request (the
//! one that serves the api's root and a named one), HOW the request addresses the api (the
//! api's own `execute-api` host, that host with the root stage's name spelled out, the local
//! endpoint marker with and without that name, the api id as the first path segment, and the
//! stage segment alone), the SHAPE of the route key the path matches (a literal path, a path
//! variable, the greedy remainder and the `$default` catch-all), the number of path segments,
//! and the payload format version the event is built in (both values the model admits).
//!
//! The event is read off what the FUNCTION returned — the echo handler
//! `http_api_backend_dispatch` shares — so nothing asserted here is a value this emulator
//! filled in.
//!
//! The second table below holds the same rule one step further along, where a client reads it:
//!
//! > **A handler that ANSWERS FROM the path the event handed it answers the path the client
//! > asked for — the api's own invoke URL path, with no stage segment at all for the stage
//! > serving the api's root — and a SIBLING route of the same deployment that cannot produce a
//! > backend response is the gateway's own `500` `{"message":"Internal Server Error"}`, never
//! > the `502` the REST data plane answers a malformed proxy response with, which changes
//! > nothing about what the healthy route's function observes.**
//!
//! Why both: a handler that DISPATCHES on the path — every framework adapter, and every
//! hand-written path comparison — composes its answer out of it, so a path wrong by one segment
//! makes the function answer for a route the client never asked for, and the healthy leg of a
//! check about a REFUSED sibling route cannot be established at all. The fault classes
//! themselves are owned by `http_api_backend_dispatch.rs`,
//! `http_api_integration_response_mapping.rs` and `http_api_lambda_invoke_permission.rs`, each
//! on an api of its own; what is only here is the two legs TOGETHER on one deployment.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, given_api, given_function, given_ungranted_function,
    lambda_integration, ECHO_EVENT,
};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The stage that serves an api's root, reached with no stage segment in the path at all.
const ROOT_STAGE: &str = "$default";

/// The marker a rewritten endpoint keeps the api's own endpoint behind, as
/// `service::data_plane::LOCAL_ENDPOINT_PREFIX` spells it.
const LOCAL_MARKER: &str = "/_aws/execute-api";

/// An address of the emulator's own endpoint, which names no service: what a client that
/// cannot resolve the api's host sends to.
const LOCAL_HOST: &str = "127.0.0.1:9297";

/// How the request reaches the emulator — the addressings `service::invoke` resolves.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// The api's own `ApiEndpoint` host, as `Api$ApiEndpoint` documents it: the invoke URL
    /// path and nothing else.
    ApiHost,
    /// The api's own host with the stage's name spelled out as the first segment, which is
    /// how a client that appends every stage name reaches the root stage here.
    ApiHostNamingTheStage,
    /// The emulator's own endpoint, with the api's endpoint behind the local marker.
    LocalMarker,
    /// The same, with the stage's name spelled out too.
    LocalMarkerNamingTheStage,
    /// The emulator's own endpoint, with the api id as the first path segment.
    ApiIdFirst,
    /// The emulator's own endpoint, with the stage segment alone naming the deployment.
    StageOnly,
}

/// One row: the stage the request is served by, the route the api declares, the path within
/// that stage the request asks for, the addressing it arrives by, and the payload format the
/// event is built in.
struct Row {
    case: &'static str,
    stage: &'static str,
    route_key: &'static str,
    /// The path within the stage — what the route key matches.
    route_path: &'static str,
    addressing: Addressing,
    payload_format: &'static str,
}

/// The path the api's own endpoint is addressed at for `route_path` of `stage`: the rule the
/// rows are held to, stated once — a named stage's name ahead of the path, and nothing ahead
/// of it for the stage that serves the api's root.
fn endpoint_path(stage: &str, route_path: &str) -> String {
    if stage == ROOT_STAGE {
        route_path.to_string()
    } else {
        format!("/{stage}{route_path}")
    }
}

/// The host and path the row's addressing sends the request to.
fn addressed(row: &Row, api_id: &str, endpoint_host: &str) -> (String, String) {
    let aws_path = endpoint_path(row.stage, row.route_path);
    let staged = format!("/{}{}", row.stage, row.route_path);
    match row.addressing {
        Addressing::ApiHost => (endpoint_host.to_string(), aws_path),
        Addressing::ApiHostNamingTheStage => (endpoint_host.to_string(), staged),
        Addressing::LocalMarker => (
            LOCAL_HOST.to_string(),
            format!("{LOCAL_MARKER}/{api_id}{aws_path}"),
        ),
        Addressing::LocalMarkerNamingTheStage => (
            LOCAL_HOST.to_string(),
            format!("{LOCAL_MARKER}/{api_id}{staged}"),
        ),
        Addressing::ApiIdFirst => (LOCAL_HOST.to_string(), format!("/{api_id}{aws_path}")),
        Addressing::StageOnly => (LOCAL_HOST.to_string(), staged),
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the root stage on the api's own host",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::ApiHost,
            payload_format: "2.0",
        },
        Row {
            case: "the root stage reached with its own name as a path segment",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::ApiHostNamingTheStage,
            payload_format: "2.0",
        },
        Row {
            case: "the root stage behind the local endpoint marker",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::LocalMarker,
            payload_format: "2.0",
        },
        Row {
            case: "the root stage behind the marker, with its own name too",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::LocalMarkerNamingTheStage,
            payload_format: "2.0",
        },
        Row {
            case: "the root stage with the api id as the first segment",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::ApiIdFirst,
            payload_format: "2.0",
        },
        Row {
            case: "the root stage named by its segment alone",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::StageOnly,
            payload_format: "2.0",
        },
        Row {
            case: "the root stage, a path variable and several segments",
            stage: ROOT_STAGE,
            route_key: "GET /orders/{id}/items",
            route_path: "/orders/7/items",
            addressing: Addressing::StageOnly,
            payload_format: "2.0",
        },
        Row {
            case: "the root stage and the greedy remainder, under 1.0",
            stage: ROOT_STAGE,
            route_key: "ANY /files/{proxy+}",
            route_path: "/files/a/b/c",
            addressing: Addressing::LocalMarkerNamingTheStage,
            payload_format: "1.0",
        },
        Row {
            case: "the root stage and the $default catch-all route, under 1.0",
            stage: ROOT_STAGE,
            route_key: "$default",
            route_path: "/anything/at/all",
            addressing: Addressing::StageOnly,
            payload_format: "1.0",
        },
        Row {
            case: "the root stage at the api's root path",
            stage: ROOT_STAGE,
            route_key: "$default",
            route_path: "/",
            addressing: Addressing::ApiHostNamingTheStage,
            payload_format: "2.0",
        },
        Row {
            case: "a named stage on the api's own host",
            stage: "prod",
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::ApiHost,
            payload_format: "2.0",
        },
        Row {
            case: "a named stage behind the local endpoint marker",
            stage: "prod",
            route_key: "GET /orders/{id}",
            route_path: "/orders/7",
            addressing: Addressing::LocalMarker,
            payload_format: "2.0",
        },
        Row {
            case: "a named stage with the api id as the first segment, under 1.0",
            stage: "prod",
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::ApiIdFirst,
            payload_format: "1.0",
        },
        Row {
            case: "a named stage named by its segment alone",
            stage: "staging",
            route_key: "ANY /files/{proxy+}",
            route_path: "/files/a/b",
            addressing: Addressing::StageOnly,
            payload_format: "2.0",
        },
    ]
}

/// The two members of the event that carry the path, in the payload format's own spelling.
///
/// `pub(crate)` because the access-log suite
/// ([`crate::http_api_access_log_request_path`]) reads the event's path through the SAME
/// spelling when it holds the record and the event to one admitted path, rather than keeping a
/// second copy of this mapping that could drift from it.
pub(crate) fn path_members(payload_format: &str) -> [(&'static str, &'static str); 2] {
    if payload_format == "2.0" {
        [
            ("the raw path", "/rawPath"),
            ("the request context's path", "/requestContext/http/path"),
        ]
    } else {
        [
            ("the path", "/path"),
            ("the request context's path", "/requestContext/path"),
        ]
    }
}

#[tokio::test]
async fn the_event_reports_the_path_the_apis_own_endpoint_was_addressed_at() {
    for row in rows() {
        let case = row.case;
        let (emulator, engine) = emulator_with_lambda();
        let function = given_function(&emulator, &engine, case, "event-path-function", ECHO_EVENT)
            .await;
        let (api_id, endpoint_host) = given_api(
            &emulator,
            case,
            lambda_integration(&function, row.payload_format),
            row.route_key,
            serde_json::json!({ "stageName": row.stage, "autoDeploy": true }),
        )
        .await;
        let (host, path) = addressed(&row, &api_id, &endpoint_host);
        let answer = send(&emulator, &host, "GET", &path, &[]).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected the function's 200 for {path:?}, got {}",
            answer.summary()
        );
        let event: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
        let expected = endpoint_path(row.stage, row.route_path);
        for (member, pointer) in path_members(row.payload_format) {
            let actual = event.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual,
                serde_json::json!(expected),
                "case {case:?}: expected {member} ({pointer}) of the event for {path:?} to be \
                 {expected:?}, got {actual:?}"
            );
        }
        // Whatever the addressing carried, the event's own path members name neither the
        // root stage nor anything the emulator's addressing added: the stage name belongs on
        // `requestContext.stage`, and the api id nowhere in a path.
        if row.stage == ROOT_STAGE {
            for pointer in path_members(row.payload_format).map(|(_, pointer)| pointer) {
                let actual = event
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !actual.contains(ROOT_STAGE) && !actual.contains(&api_id),
                    "case {case:?}: expected {pointer} to carry neither the stage name nor the \
                     api id, got {actual:?}"
                );
            }
        }
    }
}

/// The status and sentence an HTTP api answers for a route whose integration produced no
/// response — the class every sibling fault of the second table falls into.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// The status the REST data plane answers for a malformed Lambda proxy response. An HTTP api
/// maps every integration fault to [`INTEGRATION_FAULT`], so a sibling route is never answered
/// this.
const BAD_GATEWAY: u16 = 502;

/// The status a route whose backend RAN and did not answer within its own deadline is refused
/// with: the transient class a FIRST invocation can meet while that function's runtime is
/// still starting, which [`answered`] looks past rather than asserts on.
const TIMED_OUT: u16 = 503;

/// The route every row's SIBLING fault sits behind, and the path within the stage that reaches
/// it. A literal key, so it is matched ahead of a row's greedy or `$default` healthy route.
const FAULT_ROUTE_KEY: &str = "GET /fault";
const FAULT_ROUTE_PATH: &str = "/fault";

/// What a healthy row's handler prefixes its answer with, so the body is read as a string the
/// FUNCTION composed rather than as a path echoed by something else.
const ANSWERED_BY: &str = "handler-answered";

/// How long a row keeps looking for the route to be answered by a backend that could RUN, and
/// how often it looks. The deadline is generous: the CONDITION decides the outcome, never how
/// fast the host runs, and nothing here asserts on the clock.
const BACKEND_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// Which fault the sibling route of a row's deployment meets — the three ways an `AWS_PROXY`
/// route produces no backend response.
#[derive(Debug, Clone, Copy)]
enum Fault {
    /// A handler that raised, so the invocation produced nothing.
    Raises,
    /// A handler whose answer the payload format cannot be read as a proxy response.
    Malformed,
    /// A function whose resource-based policy does not admit the gateway's invocation, so the
    /// function is never invoked at all.
    NotPermitted,
}

/// The handler a healthy row runs: it ANSWERS FROM the path the event handed it, read through
/// the member that format states the path in, so what the client reads is a string the function
/// composed out of what it observed.
///
/// `get` rather than subscription, so a format that stated the path somewhere else is a visible
/// answer rather than a raise the gateway would report as its own fault.
fn answering_handler(member: &str) -> String {
    format!(
        "def lambda_handler(event, context):\n\
         \x20   return {{'statusCode': 200,\n\
         \x20            'headers': {{'content-type': 'text/plain'}},\n\
         \x20            'body': '{ANSWERED_BY}:' + str(event.get('{member}'))}}\n"
    )
}

/// The member a proxy event of `payload_format` states the path in, as the handler names it:
/// the first of the members [`path_members`] maps, so the two tables read ONE mapping.
fn path_member(payload_format: &str) -> &'static str {
    path_members(payload_format)[0].1.trim_start_matches('/')
}

/// A handler that raises, so the invocation produces no response at all.
const RAISING_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   raise RuntimeError('this route fails')\n";

/// A handler whose answer names a status no HTTP response can carry, which neither payload
/// format can be read as a proxy response.
const MALFORMED_HANDLER: &str = "def lambda_handler(event, context):\n\
     \x20   return {'statusCode': 99}\n";

/// The functions the second table shares: one answering handler per payload format, and one per
/// fault class. Created once, so a row costs one api rather than one runtime.
struct Functions {
    answering_2_0: String,
    answering_1_0: String,
    raising: String,
    malformed: String,
    not_permitted: String,
}

impl Functions {
    /// The ARN of the function a healthy route of `payload_format` fronts.
    fn answering(&self, payload_format: &str) -> &str {
        if payload_format == "2.0" {
            &self.answering_2_0
        } else {
            &self.answering_1_0
        }
    }

    /// The ARN of the function the sibling route of `fault` fronts.
    fn faulting(&self, fault: Fault) -> &str {
        match fault {
            Fault::Raises => &self.raising,
            Fault::Malformed => &self.malformed,
            Fault::NotPermitted => &self.not_permitted,
        }
    }
}

/// Create every function the second table shares. The last one is deliberately left WITHOUT the
/// resource-based grant an api's invocation needs, which is the [`Fault::NotPermitted`] class.
async fn given_functions(
    emulator: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
) -> Functions {
    Functions {
        answering_2_0: given_function(
            emulator,
            engine,
            case,
            "answering-path-2-0",
            &answering_handler(path_member("2.0")),
        )
        .await,
        answering_1_0: given_function(
            emulator,
            engine,
            case,
            "answering-path-1-0",
            &answering_handler(path_member("1.0")),
        )
        .await,
        raising: given_function(emulator, engine, case, "answering-path-raises", RAISING_HANDLER)
            .await,
        malformed: given_function(
            emulator,
            engine,
            case,
            "answering-path-malformed",
            MALFORMED_HANDLER,
        )
        .await,
        not_permitted: given_ungranted_function(
            emulator,
            engine,
            case,
            "answering-path-ungranted",
            &answering_handler(path_member("2.0")),
        )
        .await,
    }
}

/// One api carrying `routes` — each `(route key, function ARN)` behind a Lambda proxy
/// integration of `payload_format` — released to `stage` with auto-deploy on. Answers
/// `(ApiId, the host its `ApiEndpoint` names)`.
///
/// `http_api_backend_dispatch::given_api` is this for ONE route, and is not generalised into
/// this because the ORDER differs and the difference is what a row asserts on: every route here
/// exists before the stage is created, so both legs belong to the one deployment that stage
/// released, rather than the sibling arriving as a later republish of an already-serving stage
/// (which `auto_deploy_republishes.rs` owns). A row is then about the two legs of one frozen
/// deployment, with no republish between them.
async fn deployed(
    emulator: &MultiServiceRouter,
    case: &str,
    stage: &str,
    payload_format: &str,
    routes: &[(&str, &str)],
) -> (String, String) {
    let (status, api) = control(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "answered-path-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = api
        .get("apiId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let host = api
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_default();
    for (route_key, function_arn) in routes {
        let (status, created) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(lambda_integration(function_arn, payload_format)),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateIntegration for {route_key:?} answered {status} {created}"
        );
        let integration_id = created
            .get("integrationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (status, created) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": route_key,
                "target": format!("integrations/{integration_id}")
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateRoute {route_key:?} answered {status} {created}"
        );
    }
    let (status, created) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": stage, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage {stage:?} answered {status} {created}"
    );
    (api_id, host)
}

/// The answer to one invoke once a backend that could RUN produced it: the request is made
/// again while the gateway is still answering a class a STARTING runtime can produce and
/// `expected` is not that class itself (see [`BACKEND_DEADLINE`]).
async fn answered(
    emulator: &MultiServiceRouter,
    host: &str,
    path: &str,
    expected: u16,
) -> Answer {
    let deadline = Instant::now() + BACKEND_DEADLINE;
    loop {
        let answer = send(emulator, host, "GET", path, &[]).await;
        let transient = answer.status != expected
            && (answer.status == INTEGRATION_FAULT || answer.status == TIMED_OUT);
        if !transient || Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The row of the second table whose healthy route sits at `route_path`, beside a sibling route
/// meeting `fault`.
fn answering_rows() -> Vec<(Row, Fault)> {
    vec![
        (
            Row {
                case: "the root stage, a nested literal path, 2.0, beside a handler that raised",
                stage: ROOT_STAGE,
                route_key: "GET /v1/ok",
                route_path: "/v1/ok",
                addressing: Addressing::ApiHost,
                payload_format: "2.0",
            },
            Fault::Raises,
        ),
        (
            Row {
                case: "the root stage, a nested literal path, 1.0, beside a malformed response",
                stage: ROOT_STAGE,
                route_key: "GET /v1/ok",
                route_path: "/v1/ok",
                addressing: Addressing::ApiHost,
                payload_format: "1.0",
            },
            Fault::Malformed,
        ),
        (
            Row {
                case: "the root stage, one literal segment, 2.0, beside a route no policy admits",
                stage: ROOT_STAGE,
                route_key: "GET /granted",
                route_path: "/granted",
                addressing: Addressing::ApiHost,
                payload_format: "2.0",
            },
            Fault::NotPermitted,
        ),
        (
            Row {
                case: "the root stage reached through the emulator's own endpoint, 1.0",
                stage: ROOT_STAGE,
                route_key: "GET /granted",
                route_path: "/granted",
                addressing: Addressing::ApiIdFirst,
                payload_format: "1.0",
            },
            Fault::Raises,
        ),
        (
            Row {
                case: "the root stage and the greedy remainder, several segments, 2.0",
                stage: ROOT_STAGE,
                route_key: "ANY /files/{proxy+}",
                route_path: "/files/a/b/c",
                addressing: Addressing::LocalMarker,
                payload_format: "2.0",
            },
            Fault::Malformed,
        ),
        (
            Row {
                case: "the root stage and the $default catch-all route, 1.0",
                stage: ROOT_STAGE,
                route_key: "$default",
                route_path: "/anything/at/all",
                // Named behind the marker rather than by its stage segment alone: these rows
                // share one emulator, so an addressing that carries no api id would name a
                // stage several of their apis hold.
                addressing: Addressing::LocalMarkerNamingTheStage,
                payload_format: "1.0",
            },
            Fault::NotPermitted,
        ),
        (
            Row {
                case: "a named stage, a path variable, 2.0",
                stage: "prod",
                route_key: "GET /orders/{id}",
                route_path: "/orders/7",
                addressing: Addressing::ApiHost,
                payload_format: "2.0",
            },
            Fault::Raises,
        ),
        (
            Row {
                case: "a named stage reached through the emulator's own endpoint, 1.0",
                stage: "live",
                route_key: "GET /v1/ok",
                route_path: "/v1/ok",
                addressing: Addressing::ApiIdFirst,
                payload_format: "1.0",
            },
            Fault::Malformed,
        ),
    ]
}

/// The sibling route of `row`'s deployment, addressed the same way: the row's own stage and
/// addressing, at [`FAULT_ROUTE_PATH`].
fn fault_leg(row: &Row) -> Row {
    Row {
        case: row.case,
        stage: row.stage,
        route_key: FAULT_ROUTE_KEY,
        route_path: FAULT_ROUTE_PATH,
        addressing: row.addressing,
        payload_format: row.payload_format,
    }
}

#[tokio::test]
async fn a_function_answers_from_the_path_the_apis_own_endpoint_was_addressed_at() {
    let (emulator, engine) = emulator_with_lambda();
    let functions = given_functions(&emulator, &engine, "the shared functions").await;
    for (row, fault) in answering_rows() {
        let case = row.case;
        let (api_id, endpoint_host) = deployed(
            &emulator,
            case,
            row.stage,
            row.payload_format,
            &[
                (row.route_key, functions.answering(row.payload_format)),
                (FAULT_ROUTE_KEY, functions.faulting(fault)),
            ],
        )
        .await;

        // The healthy leg: the function composed its answer out of the path it observed, and
        // that path is the one the api's own endpoint was addressed at.
        let expected_body = format!("{ANSWERED_BY}:{}", endpoint_path(row.stage, row.route_path));
        let (host, path) = addressed(&row, &api_id, &endpoint_host);
        let answer = answered(&emulator, &host, &path, 200).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected the function's 200 for {path:?}, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.text, expected_body,
            "case {case:?}: expected the function to answer {expected_body:?} for {path:?}, got \
             {:?}",
            answer.text
        );
        if row.stage == ROOT_STAGE {
            assert!(
                !answer.text.contains(ROOT_STAGE) && !answer.text.contains(&api_id),
                "case {case:?}: expected the path the function answered from to carry neither \
                 the root stage's name nor the api id, got {:?}",
                answer.text
            );
        }

        // The sibling leg: the same deployment's other route cannot produce a backend
        // response, and the gateway owns that answer.
        let (fault_host, fault_path) = addressed(&fault_leg(&row), &api_id, &endpoint_host);
        let refused = answered(&emulator, &fault_host, &fault_path, INTEGRATION_FAULT).await;
        assert_eq!(
            refused.status,
            INTEGRATION_FAULT,
            "case {case:?}: expected {INTEGRATION_FAULT} for the {fault:?} sibling route \
             {fault_path:?} — an HTTP api maps every integration fault to it, and never to the \
             {BAD_GATEWAY} the REST plane answers a malformed proxy response with — got {}",
            refused.summary()
        );
        assert_eq!(
            message(&refused),
            INTEGRATION_FAULT_MESSAGE,
            "case {case:?}: expected the integration-fault sentence for the {fault:?} sibling \
             route, got {}",
            refused.summary()
        );

        // And the fault changed nothing about what the healthy route's function observes: the
        // control leg of a check about the refusal still reads the client's own path.
        let again = answered(&emulator, &host, &path, 200).await;
        assert_eq!(
            (again.status, again.text.as_str()),
            (200, expected_body.as_str()),
            "case {case:?}: expected the healthy route to answer {expected_body:?} again after \
             the sibling fault, got {}",
            again.summary()
        );
    }
}
