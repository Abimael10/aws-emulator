// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the FORM of an api's `ApiEndpoint` reported to a client that
//! resolves AWS's OWN hostnames.
//!
//! The rule, one level above any single deployment:
//!
//! > An api created or imported by a client that resolves AWS's own hostnames reports, as
//! > its `ApiEndpoint`, the api's own default endpoint in the form that member documents —
//! > "The URI of the API, of the form `{api-id}.execute-api.{region}.amazonaws.com`"
//! > (`Api$ApiEndpoint`, and `Api$DisableExecuteApiEndpoint` repeats it as "the default
//! > https://{api_id}.execute-api.{region}.amazonaws.com") — so the value is the protocol's
//! > scheme, then a host whose FIRST label is the api id returned in the same response and
//! > whose remaining labels are the literal `execute-api`, the Region the request was
//! > SIGNED for and `amazonaws.com`, and no path at all. Only the id and the Region vary,
//! > which is what lets such a client recover both from the host, and this emulator's front
//! > door claims the host it names.
//!
//! The rows span what the rule generalises over: WHICH of AWS's own endpoint hosts the
//! control-plane request was addressed at, the Region the request is signed for (including
//! the emulator's own default, so a row cannot pass on a hardcoded Region), the operation
//! that minted the api (`CreateApi`, `ImportApi`) and the operation that reported the value
//! back (`GetApi`, `GetApis`, `UpdateApi`, `ReimportApi`).
//!
//! A client that resolves nothing but this emulator's own endpoint is announced something
//! else — the endpoint it reached, with the api behind the reserved data-plane path, since a
//! URL it cannot dial publishes nothing. That is `http_api_published_invoke_address`'s rule;
//! this suite holds the half of it that this form is for.
//!
//! One WEBSOCKET row pins the single api neither rule covers: a WebSocket api's data plane
//! is a socket of its own — an RFC 6455 handshake is completed against that socket and
//! nowhere else — so such an api reports the `wss` address the core opened for it. That
//! endpoint's own rule is `api_endpoint_scheme`'s; this suite only holds it fixed.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call_at_host, emulator};
use crate::websocket_handshake_data_plane::{send, Answer};

/// The status an `HTTP_PROXY` route answers: the upstream is a URI the CALLER can reach, so
/// the caller is sent there — `307`, with the integration's own `IntegrationUri` in
/// `Location`. Reaching it proves the request resolved all the way to the route's
/// integration, which is what the announced host being CLAIMED means.
const SERVED: u16 = 307;

/// The front door's envelope for a request no service claimed.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The upstream every row's `HTTP_PROXY` integration fronts.
const UPSTREAM: &str = "https://upstream.example.com/items";

/// The route every row deploys, and the path within the stage that matches it.
const ROUTE_KEY: &str = "GET /items";
const ROUTE_PATH: &str = "/items";

/// The reserved stage "that serves an api's root", reached with no stage segment at all.
const DEFAULT_STAGE: &str = "$default";

/// One row: the AWS endpoint host the control-plane calls are addressed at and the Region
/// they are signed for. The announced endpoint is the same documented form for every row.
struct Row {
    case: &'static str,
    /// The `Host` the control-plane request carries — one of AWS's own endpoint hosts, which
    /// is the client this form is reported to.
    host: &'static str,
    region: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client that resolves AWS's own hosts: this service's endpoint host",
            host: "apigateway.us-west-2.amazonaws.com",
            region: "us-west-2",
        },
        Row {
            case: "the same client in another Region",
            host: "apigateway.eu-west-1.amazonaws.com",
            region: "eu-west-1",
        },
        Row {
            case: "a Region whose own name carries three labels",
            host: "apigateway.ap-southeast-2.amazonaws.com",
            region: "ap-southeast-2",
        },
        Row {
            case: "the emulator's own default Region, signed for explicitly",
            host: "apigateway.us-east-1.amazonaws.com",
            region: "us-east-1",
        },
    ]
}

/// The documented endpoint of an HTTP api: the scheme an HTTP api is invoked over, the api's
/// own id as the host's first label, then `execute-api`, the Region and `amazonaws.com`.
fn documented_endpoint(api_id: &str, region: &str) -> String {
    format!("https://{api_id}.execute-api.{region}.amazonaws.com")
}

/// Assert `reported` is the documented endpoint of `api_id` in `row`'s Region, part by part,
/// so a failure says WHICH part of the form is wrong.
fn assert_announced(row: &Row, reporter: &str, api_id: &str, reported: &str) {
    let expected = documented_endpoint(api_id, row.region);
    assert_eq!(
        reported, expected,
        "case {:?}: {reporter} must report {expected:?}, got {reported:?}",
        row.case
    );
    let (authority, path) = split_authority(row, reporter, reported);
    assert!(
        path.is_empty(),
        "case {:?}: the documented endpoint carries no path of its own, got {reported:?}",
        row.case
    );
    let labels: Vec<&str> = authority.split('.').collect();
    assert_eq!(
        labels.first().copied(),
        Some(api_id),
        "case {:?}: {reporter} must carry the api id as the host's first label, got \
         {reported:?}",
        row.case
    );
    assert_eq!(
        labels.get(1).copied(),
        Some("execute-api"),
        "case {:?}: {reporter} must name the execute-api endpoint, got {reported:?}",
        row.case
    );
    assert_eq!(
        labels.get(2).copied(),
        Some(row.region),
        "case {:?}: {reporter} must carry the request's Region, got {reported:?}",
        row.case
    );
}

/// `reported` split into the authority a client dials and the path it carries — empty for an
/// endpoint that is a bare host.
fn split_authority(row: &Row, reporter: &str, reported: &str) -> (String, String) {
    let authority = reported
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {:?}: {reporter} reported {reported:?}", row.case));
    match authority.find('/') {
        Some(at) => (authority[..at].to_string(), authority[at..].to_string()),
        None => (authority.to_string(), String::new()),
    }
}

/// The OpenAPI definition the import rows PUT: one route fronting the same upstream, so what
/// an import and a reimport are asserted on is the endpoint they report.
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
/// `$default` stage, so the endpoint it reports can be invoked.
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

/// Send one invoke to the announced address and assert the api's route SERVED it: the
/// authority the announced endpoint names, at its own base path followed by `path`.
async fn assert_invoke_reaches_the_route(
    emulator: &MultiServiceRouter,
    row: &Row,
    announced: &str,
    path: &str,
) {
    let (authority, base_path) = split_authority(row, "the announced endpoint", announced);
    let path = format!("{base_path}{path}");
    let answer: Answer = send(emulator, &authority, "GET", &path, &[]).await;
    assert_ne!(
        answer.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case {:?}: the announced host must be one this emulator claims, but \
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

/// `CreateApi` reports the documented endpoint to a client that resolves AWS's own hosts,
/// every later reporter repeats it, and the host it names is one this emulator claims.
#[tokio::test]
async fn a_created_api_reports_the_documented_execute_api_endpoint() {
    for (index, row) in rows().into_iter().enumerate() {
        // One emulator per row, so GetApis lists exactly this row's api.
        let emulator = emulator();
        let name = format!("documented-endpoint-{index}");

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
        assert_announced(&row, "CreateApi", &api_id, &announced);

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
            Some(json!({ "description": "documented-endpoint check" })),
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

        // And the address that endpoint names is claimed: the reimported definition's route
        // answers on it, both at the api's root stage and with that stage spelled out.
        let (status, stage) = call_at_host(
            &emulator,
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
        assert_invoke_reaches_the_route(&emulator, &row, &announced, ROUTE_PATH).await;
        assert_invoke_reaches_the_route(
            &emulator,
            &row,
            &announced,
            &format!("/{DEFAULT_STAGE}{ROUTE_PATH}"),
        )
        .await;
    }
}

/// `ImportApi` reports the documented endpoint for the api id the import minted, and the
/// route the definition declared answers on it.
#[tokio::test]
async fn an_imported_api_reports_the_documented_execute_api_endpoint() {
    for (index, row) in rows().into_iter().enumerate() {
        let emulator = emulator();
        let name = format!("documented-endpoint-import-{index}");

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
        assert_announced(&row, "ImportApi", &api_id, &announced);

        // An imported api carries no stage, so one is released over it, and the endpoint the
        // import published serves the route the definition declared.
        let (status, stage) = call_at_host(
            &emulator,
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
        assert_invoke_reaches_the_route(&emulator, &row, &announced, ROUTE_PATH).await;
    }
}

/// A route created over a CREATED api answers on the endpoint that api published too: the
/// announced host is the api's whichever way the api got its routes.
#[tokio::test]
async fn the_announced_host_serves_a_created_api_s_own_route() {
    let row = &rows()[0];
    let emulator = emulator();

    let (status, created) = call_at_host(
        &emulator,
        row.host,
        row.region,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "documented-endpoint-served", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: CreateApi must answer 201, got {status} {created}",
        row.case
    );
    let api_id = as_str(&created, "apiId");
    let announced = as_str(&created, "apiEndpoint");
    assert_announced(row, "CreateApi", &api_id, &announced);

    deploy(&emulator, row, &api_id).await;
    assert_invoke_reaches_the_route(&emulator, row, &announced, ROUTE_PATH).await;
}

/// The api neither half of this rule covers: a WEBSOCKET api reports the `wss` socket the
/// core opened for its data plane, which is the only address an RFC 6455 handshake can be
/// completed against. `ProtocolType` is `HTTP | WEBSOCKET`
/// (`smithy_query(query="shape", shape="ProtocolType")`), so this is the whole exception.
#[tokio::test]
async fn a_websocket_api_still_reports_the_socket_its_data_plane_was_opened_on() {
    for row in rows() {
        let emulator = emulator();
        let (status, created) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            "/v2/apis",
            Some(json!({
                "name": "documented-endpoint-websocket",
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
                "case {:?}: a WEBSOCKET api must report a wss endpoint, got {announced:?}",
                row.case
            )
        });
        let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
            panic!(
                "case {:?}: a WEBSOCKET api must report the host:port of its socket, got \
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
                    "case {:?}: a WEBSOCKET api must report the address its socket is bound \
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

/// Only the api id and the Region vary: every api announced to a client that resolves AWS's
/// own hosts carries the same fixed labels after its own id, and the Region each carries is
/// the one its create was SIGNED for.
#[tokio::test]
async fn the_documented_endpoint_varies_only_in_the_api_id_and_the_region() {
    let mut announced: Vec<(String, String)> = Vec::new();
    for row in rows() {
        let emulator = emulator();
        let (status, created) = call_at_host(
            &emulator,
            row.host,
            row.region,
            "POST",
            "/v2/apis",
            Some(json!({ "name": "documented-endpoint-template", "protocolType": "HTTP" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateApi must answer 201, got {status} {created}",
            row.case
        );
        let api_id = as_str(&created, "apiId");
        let reported = as_str(&created, "apiEndpoint");
        assert_announced(&row, "CreateApi", &api_id, &reported);
        announced.push((row.region.to_string(), reported));
    }
    // What is left of each value once its own id and its own Region are taken out is the
    // template: the scheme, the `execute-api` label and `amazonaws.com`.
    let forms: Vec<String> = announced
        .iter()
        .map(|(region, endpoint)| {
            endpoint
                .split_once('.')
                .map(|(_id, rest)| rest.replace(region, "{region}"))
                .unwrap_or_else(|| endpoint.clone())
        })
        .collect();
    let first = forms.first().expect("one row per Region");
    assert!(
        forms.iter().all(|form| form == first),
        "every endpoint must be the one documented template, got {announced:?}"
    );
}
