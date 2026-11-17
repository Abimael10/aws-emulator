// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the ADDRESS an api's `ApiEndpoint` announces.
//!
//! The rule, one level above the recorded case (an HTTP api created through a client
//! configured with this emulator's own endpoint, whose `apiEndpoint` named a host that
//! client resolves nowhere):
//!
//! > `ApiEndpoint` is the URI a caller hands to a client library to reach the api, so it
//! > names an address that caller can DIAL and this emulator answers on: a request that
//! > addressed the EMULATOR'S OWN endpoint comes from a client that resolves nothing else,
//! > so the api is announced on that same endpoint with its own id behind the reserved
//! > data-plane path segment, and a request that addressed one of AWS's own hosts comes
//! > from a client that resolves them, so the api is announced on its own
//! > `{api-id}.execute-api.{region}.amazonaws.com` host — the form `Api$ApiEndpoint`
//! > documents, which this emulator's front door claims too. The scheme is the one the
//! > api's `ProtocolType` names, and whichever operation reports the api reports the one
//! > value.
//!
//! The rows span what the rule generalises over: the endpoint the control-plane request was
//! addressed at (an IPv4 loopback literal with a port, a loopback NAME on another port, a
//! bracketed IPv6 literal, and this service's own AWS host), the Region the request is
//! signed for (including the emulator's own default, so a row cannot pass on a hardcoded
//! Region), the operation that minted the api (`CreateApi`, `ImportApi`) and the operation
//! that reported the value back (`GetApi`, `GetApis`, `UpdateApi`, `ReimportApi`). The
//! recorded case is ONE row.
//!
//! One WEBSOCKET row pins the api whose data plane is a socket of its own — an RFC 6455
//! handshake is completed against that socket and nowhere else — so such an api announces
//! the `wss` address the core opened for it. That endpoint's own rule is
//! `api_endpoint_scheme`'s; this suite only holds it fixed.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call_at_host, emulator};
use crate::http_api_backend_dispatch::{emulator_with_lambda, given_function, lambda_integration};
use crate::websocket_handshake_data_plane::{send, send_with_body, Answer};

/// The status an `HTTP_PROXY` route answers: the upstream is a URI the CALLER can reach, so
/// the caller is sent there — `307`, with the integration's own `IntegrationUri` in
/// `Location`. Reaching it proves the request resolved all the way to the route's
/// integration, which is what an announced address being SERVED means.
const SERVED: u16 = 307;

/// The front door's envelope for a request no service claimed. An address nothing serves is
/// the defect this suite pins, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The upstream every row's `HTTP_PROXY` integration fronts.
const UPSTREAM: &str = "https://upstream.example.com/items";

/// The route every row deploys, and the path within the stage that matches it.
const ROUTE_KEY: &str = "GET /items";
const ROUTE_PATH: &str = "/items";

/// The reserved stage "that serves an api's root", reached with no stage segment at all.
const DEFAULT_STAGE: &str = "$default";

/// WHICH address a row's client can dial, and therefore the form the announcement takes.
enum AnnouncedOn {
    /// A client configured with one local endpoint URL: it resolves the emulator's own
    /// address and nothing else, so the api is announced there, behind the reserved
    /// data-plane marker (`/_aws/execute-api/{api-id}`, the path this emulator serves an
    /// api's data plane at).
    TheEmulatorsOwnEndpoint,
    /// A client that resolves AWS's own hosts: the api is announced on its own documented
    /// `execute-api` host, which the front door claims host-style.
    TheApisAwsHost,
}

/// One row: the endpoint the control-plane calls are addressed at, the Region they are
/// signed for, and the address form the api is therefore announced on.
struct Row {
    case: &'static str,
    /// The `Host` the control-plane request carries — the endpoint the client is configured
    /// with.
    host: &'static str,
    region: &'static str,
    announced_on: AnnouncedOn,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: a client configured with the emulator's loopback \
                   endpoint",
            host: "127.0.0.1:40705",
            region: "us-west-2",
            announced_on: AnnouncedOn::TheEmulatorsOwnEndpoint,
        },
        Row {
            case: "the same endpoint named by its loopback NAME on another port",
            host: "localhost:9297",
            region: "eu-west-1",
            announced_on: AnnouncedOn::TheEmulatorsOwnEndpoint,
        },
        Row {
            case: "the same endpoint named by a bracketed IPv6 literal",
            host: "[::1]:9297",
            region: "ap-southeast-2",
            announced_on: AnnouncedOn::TheEmulatorsOwnEndpoint,
        },
        Row {
            case: "a client that resolves AWS's own hosts: this service's own endpoint host",
            host: "apigateway.eu-north-1.amazonaws.com",
            region: "eu-north-1",
            announced_on: AnnouncedOn::TheApisAwsHost,
        },
        Row {
            case: "the emulator's own default Region, signed for explicitly",
            host: "127.0.0.1:9297",
            region: "us-east-1",
            announced_on: AnnouncedOn::TheEmulatorsOwnEndpoint,
        },
    ]
}

/// The endpoint an HTTP api of `api_id` is announced on for `row`'s client: the scheme an
/// HTTP api is invoked over, then the address that client dials.
fn expected_endpoint(row: &Row, api_id: &str) -> String {
    match row.announced_on {
        AnnouncedOn::TheEmulatorsOwnEndpoint => {
            format!("https://{}/_aws/execute-api/{api_id}", row.host)
        }
        AnnouncedOn::TheApisAwsHost => {
            format!("https://{api_id}.execute-api.{}.amazonaws.com", row.region)
        }
    }
}

/// The announced endpoint split into the authority a client dials and the path it carries
/// ahead of the stage — empty for an endpoint that is a bare host.
fn announced_parts(announced: &str) -> (String, String) {
    let without_scheme = announced
        .split_once("://")
        .map_or(announced, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), String::new()),
    }
}

/// Assert `reported` is the address `row`'s client is announced for `api_id`, and answer the
/// authority and the base path an invoke URL is built from.
fn assert_announced(row: &Row, reporter: &str, api_id: &str, reported: &str) -> (String, String) {
    let expected = expected_endpoint(row, api_id);
    assert_eq!(
        reported, expected,
        "case {:?}: {reporter} must announce {expected:?}, got {reported:?}",
        row.case
    );
    let (authority, base_path) = announced_parts(reported);
    (authority, base_path)
}

/// The OpenAPI definition the import rows PUT: one route fronting the same upstream, so what
/// an import and a reimport are asserted on is the endpoint they announce.
fn definition(name: &str) -> String {
    json!({
        "openapi": "3.0.1",
        "info": { "title": name, "version": "1.0" },
        "paths": {
            ROUTE_PATH: {
                "get": {
                    "x-amazon-apigateway-integration": {
                        "type": "http_proxy",
                        "httpMethod": "GET",
                        "uri": UPSTREAM,
                        "payloadFormatVersion": "1.0"
                    }
                }
            }
        }
    })
    .to_string()
}

/// Give the api an `HTTP_PROXY` integration, the route that fronts it and an auto-deploying
/// `$default` stage, so the endpoint it announces can be invoked.
async fn deploy(emulator: &MultiServiceRouter, row: &Row, api_id: &str) {
    let (status, integration) = call_at_host(
        emulator,
        row.host,
        row.region,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationMethod": "GET",
            "integrationUri": UPSTREAM,
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: CreateIntegration must answer 201, got {status} {integration}",
        row.case
    );
    let integration_id = as_str(&integration, "integrationId");

    let (status, route) = call_at_host(
        emulator,
        row.host,
        row.region,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": ROUTE_KEY,
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: CreateRoute must answer 201, got {status} {route}",
        row.case
    );

    create_default_stage(emulator, row, api_id).await;
}

/// Release the api's `$default` stage, which is what makes a route of it servable.
async fn create_default_stage(emulator: &MultiServiceRouter, row: &Row, api_id: &str) {
    let (status, stage) = call_at_host(
        emulator,
        row.host,
        row.region,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": DEFAULT_STAGE, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: CreateStage must answer 201, got {status} {stage}",
        row.case
    );
}

/// Send one invoke to the announced address and assert the api's route SERVED it — and,
/// first, that the front door did not answer that nothing claims that address.
async fn assert_invoke_reaches_the_route(
    emulator: &MultiServiceRouter,
    row: &Row,
    authority: &str,
    path: &str,
) {
    let answer: Answer = send(emulator, authority, "GET", path, &[]).await;
    assert_ne!(
        answer.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case {:?}: the announced address must be one this emulator claims, but \
         {authority}{path} is claimed by no service: {}",
        row.case,
        answer.summary()
    );
    assert_eq!(
        answer.status,
        SERVED,
        "case {:?}: {authority}{path} must reach the route's integration, got {}",
        row.case,
        answer.summary()
    );
    assert_eq!(
        answer.location.as_deref(),
        Some(UPSTREAM),
        "case {:?}: {authority}{path} must be sent to the upstream the integration names, \
         got {}",
        row.case,
        answer.summary()
    );
}

/// `CreateApi` announces the address the client that created the api can dial, every later
/// reporter repeats it, and that address serves the api's route.
#[tokio::test]
async fn every_reporter_announces_the_address_the_client_can_dial() {
    for (index, row) in rows().into_iter().enumerate() {
        // One emulator per row, so GetApis lists exactly this row's api.
        let emulator = emulator();
        let name = format!("announced-address-{index}");

        let (status, created) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            "/v2/apis",
            Some(json!({ "name": name, "protocolType": "HTTP" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateApi must answer 201, got {status} {created}",
            row.case
        );
        let api_id = as_str(&created, "apiId");
        let announced = as_str(&created, "apiEndpoint");
        let (authority, base_path) = assert_announced(&row, "CreateApi", &api_id, &announced);

        let (status, fetched) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "GET",
            &format!("/v2/apis/{api_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: GetApi must answer 200, got {status} {fetched}",
            row.case
        );
        assert_announced(&row, "GetApi", &api_id, &as_str(&fetched, "apiEndpoint"));

        let (status, listed) =
            call_at_host(&emulator, row.host, row.region, "GET", "/v2/apis", None).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetApis must answer 200, got {status} {listed}",
            row.case
        );
        let items = listed["items"].as_array().unwrap_or_else(|| {
            panic!("case {:?}: GetApis served no items, got {listed}", row.case)
        });
        assert_eq!(
            items.len(),
            1,
            "case {:?}: this emulator holds exactly the row's api, got {listed}",
            row.case
        );
        assert_announced(&row, "GetApis", &api_id, &as_str(&items[0], "apiEndpoint"));

        let (status, updated) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "PATCH",
            &format!("/v2/apis/{api_id}"),
            Some(json!({ "description": "announced-address check" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: UpdateApi must answer 201, got {status} {updated}",
            row.case
        );
        assert_announced(&row, "UpdateApi", &api_id, &as_str(&updated, "apiEndpoint"));

        let (status, reimported) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "PUT",
            &format!("/v2/apis/{api_id}"),
            Some(json!({ "body": definition(&name) })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: ReimportApi must answer 200, got {status} {reimported}",
            row.case
        );
        assert_announced(
            &row,
            "ReimportApi",
            &api_id,
            &as_str(&reimported, "apiEndpoint"),
        );

        // And the address that endpoint names is SERVED: the reimported definition's route
        // answers on it, both at the api's root stage and with that stage spelled out.
        create_default_stage(&emulator, &row, &api_id).await;
        assert_invoke_reaches_the_route(
            &emulator,
            &row,
            &authority,
            &format!("{base_path}{ROUTE_PATH}"),
        )
        .await;
        assert_invoke_reaches_the_route(
            &emulator,
            &row,
            &authority,
            &format!("{base_path}/{DEFAULT_STAGE}{ROUTE_PATH}"),
        )
        .await;
    }
}

/// `ImportApi` announces the same address for the api id the import minted, and the route the
/// definition declared answers on it.
#[tokio::test]
async fn an_imported_api_is_announced_on_the_address_the_client_can_dial() {
    for (index, row) in rows().into_iter().enumerate() {
        let emulator = emulator();
        let name = format!("announced-address-import-{index}");

        let (status, imported) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "PUT",
            "/v2/apis",
            Some(json!({ "body": definition(&name) })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: ImportApi must answer 200, got {status} {imported}",
            row.case
        );
        let api_id = as_str(&imported, "apiId");
        let announced = as_str(&imported, "apiEndpoint");
        let (authority, base_path) = assert_announced(&row, "ImportApi", &api_id, &announced);

        // An imported api carries no stage, so one is released over it, and the address the
        // import published serves the route the definition declared.
        create_default_stage(&emulator, &row, &api_id).await;
        assert_invoke_reaches_the_route(
            &emulator,
            &row,
            &authority,
            &format!("{base_path}{ROUTE_PATH}"),
        )
        .await;
    }
}

/// A route created over a CREATED api answers on the address that api published too: the
/// announced address is the api's whichever way the api got its routes.
#[tokio::test]
async fn the_announced_address_serves_a_created_api_s_own_route() {
    for row in rows() {
        let emulator = emulator();

        let (status, created) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            "/v2/apis",
            Some(json!({ "name": "announced-address-served", "protocolType": "HTTP" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateApi must answer 201, got {status} {created}",
            row.case
        );
        let api_id = as_str(&created, "apiId");
        let announced = as_str(&created, "apiEndpoint");
        let (authority, base_path) = assert_announced(&row, "CreateApi", &api_id, &announced);

        deploy(&emulator, &row, &api_id).await;
        assert_invoke_reaches_the_route(
            &emulator,
            &row,
            &authority,
            &format!("{base_path}{ROUTE_PATH}"),
        )
        .await;
    }
}

/// The one api this rule does not cover: a WEBSOCKET api announces the `wss` socket the core
/// opened for its data plane, which is the only address an RFC 6455 handshake can be
/// completed against. `ProtocolType` is `HTTP | WEBSOCKET`
/// (`smithy_query(query="shape", shape="ProtocolType")`), so this is the whole exception.
#[tokio::test]
async fn a_websocket_api_still_announces_the_socket_its_data_plane_was_opened_on() {
    for row in rows() {
        let emulator = emulator();
        let (status, created) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            "/v2/apis",
            Some(json!({
                "name": "announced-address-websocket",
                "protocolType": "WEBSOCKET",
                "routeSelectionExpression": "$request.body.action"
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateApi must answer 201, got {status} {created}",
            row.case
        );
        let announced = as_str(&created, "apiEndpoint");
        let authority = announced.strip_prefix("wss://").unwrap_or_else(|| {
            panic!(
                "case {:?}: a WEBSOCKET api must announce a wss endpoint, got {announced:?}",
                row.case
            )
        });
        let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
            panic!(
                "case {:?}: a WEBSOCKET api must announce the host:port of its socket, got \
                 {announced:?}",
                row.case
            )
        });
        let address: std::net::IpAddr = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .unwrap_or_else(|_| {
                panic!(
                    "case {:?}: a WEBSOCKET api must announce the address its socket is bound \
                     to, got {announced:?}",
                    row.case
                )
            });
        assert!(
            address.is_loopback(),
            "case {:?}: the socket is opened on the interface the core binds, got \
             {announced:?}",
            row.case
        );
        assert!(
            port.parse::<u16>().is_ok_and(|port| port > 0),
            "case {:?}: the socket's port is the one it landed on, got {announced:?}",
            row.case
        );
    }
}

/// The route the lifecycle rows below deploy, and the path within the stage that matches it.
const VISITOR_ROUTE_KEY: &str = "POST /visitors";
const VISITOR_PATH: &str = "/visitors";

/// The handler those rows front. It answers a document it COMPUTES from the event the gateway
/// handed it — what the client posted, the route key that matched, the method, and the source
/// address the gateway reported for the call — so what the client reads back at the announced
/// address is a function's own work and not a value this emulator filled in.
const VISITOR_HANDLER: &str = "import json\n\
     def lambda_handler(event, context):\n\
     \x20   posted = json.loads(event.get(\"body\") or \"{}\")\n\
     \x20   http = event[\"requestContext\"][\"http\"]\n\
     \x20   seen = {\"visitor\": posted.get(\"visitor\"), \"routeKey\": event[\"routeKey\"], \
     \"method\": http[\"method\"], \"sourceIp\": http[\"sourceIp\"]}\n\
     \x20   return {\"statusCode\": 200, \"headers\": {\"content-type\": \"application/json\"}, \
     \"body\": json.dumps(seen)}\n";

/// The status the gateway answers while the function a route fronts has produced no response
/// yet — which a loaded host can do transiently while that function's runtime is starting — and
/// how long a row keeps posting for the function's own answer, looking this often. Generous,
/// and bounding no behaviour the row asserts on.
const INTEGRATION_FAULT: u16 = 500;
const ANSWER_WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// POST `posted` to the announced address until the FUNCTION answered it, or [`ANSWER_WAIT`]
/// passes: the wait is for that answer — a condition — and nothing here asserts on the clock.
/// An address nothing claims, or any other status, fails the row at once.
async fn answered_by_the_function(
    emulator: &MultiServiceRouter,
    row: &Row,
    authority: &str,
    path: &str,
    posted: &Value,
) -> Answer {
    let deadline = Instant::now() + ANSWER_WAIT;
    loop {
        let answer = send_with_body(
            emulator,
            authority,
            "POST",
            path,
            &[("content-type", "application/json")],
            serde_json::to_vec(posted).expect("the posted document serializes"),
        )
        .await;
        assert_ne!(
            answer.error_type.as_deref(),
            Some(UNKNOWN_SERVICE),
            "case {:?}: the announced address must be one this emulator claims, but \
             {authority}{path} is claimed by no service: {}",
            row.case,
            answer.summary()
        );
        if answer.status == 200 {
            return answer;
        }
        assert!(
            answer.status == INTEGRATION_FAULT && Instant::now() < deadline,
            "case {:?}: {authority}{path} must be answered by the function the route fronts, \
             got {}",
            row.case,
            answer.summary()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The address an api announces is the one a SERVERLESS client drives the api at: a client that
/// read `ApiEndpoint` and posted to the route reaches the function that route fronts, and reads
/// back what that function returned.
///
/// The rows are the same ones above: the announced address is the only thing that differs
/// between a client configured with this emulator's endpoint and one that resolves AWS's own
/// hosts, and both must serve.
#[tokio::test]
async fn the_announced_address_serves_a_lambda_backed_route_with_the_functions_own_answer() {
    for (index, row) in rows().into_iter().enumerate() {
        // One emulator per row, with AWS Lambda beside this service on the same engine: the
        // route's answer belongs to a function that lives there.
        let (emulator, engine) = emulator_with_lambda();
        let function = given_function(
            &emulator,
            &engine,
            row.case,
            "announced-address-visitor",
            VISITOR_HANDLER,
        )
        .await;

        let (status, created) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            "/v2/apis",
            Some(json!({
                "name": format!("announced-address-visitor-{index}"),
                "protocolType": "HTTP"
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateApi must answer 201, got {status} {created}",
            row.case
        );
        let api_id = as_str(&created, "apiId");
        let (authority, base_path) = assert_announced(
            &row,
            "CreateApi",
            &api_id,
            &as_str(&created, "apiEndpoint"),
        );

        let (status, integration) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(lambda_integration(&function, "2.0")),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateIntegration must answer 201, got {status} {integration}",
            row.case
        );
        let integration_id = as_str(&integration, "integrationId");
        let (status, route) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": VISITOR_ROUTE_KEY,
                "target": format!("integrations/{integration_id}")
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateRoute must answer 201, got {status} {route}",
            row.case
        );
        create_default_stage(&emulator, &row, &api_id).await;

        let visitor = format!("visitor-{index}");
        let posted = json!({ "visitor": visitor });
        let answer = answered_by_the_function(
            &emulator,
            &row,
            &authority,
            &format!("{base_path}{VISITOR_PATH}"),
            &posted,
        )
        .await;
        for (what, member, expected) in [
            ("what the client posted", "visitor", json!(visitor)),
            ("the route that matched", "routeKey", json!(VISITOR_ROUTE_KEY)),
            ("the method it arrived with", "method", json!("POST")),
        ] {
            assert_eq!(
                answer.body.get(member).cloned().unwrap_or(Value::Null),
                expected,
                "case {:?}: expected {what} at {member} to be {expected}, got {}",
                row.case,
                answer.summary()
            );
        }
        assert!(
            answer
                .body
                .get("sourceIp")
                .and_then(Value::as_str)
                .is_some_and(|address| !address.trim().is_empty()),
            "case {:?}: the event must carry the source address of the call, got {}",
            row.case,
            answer.summary()
        );
    }
}
