// SPDX-License-Identifier: CC0-1.0

//! Is the address an HTTP api publishes one the client that was handed it can DIAL — without
//! asking a resolver this emulator has no answer in?
//!
//! The rule, one level above the recorded case (a stack whose `Outputs` carried
//! `Fn::GetAtt <api>.ApiEndpoint`, which the client holding them dialled):
//!
//! > `Api$ApiEndpoint` is "the URI of the API … the stage name is typically appended to this
//! > URI to form a complete path to a deployed API stage" — a value whose purpose is that a
//! > client DIALS it. So the authority this emulator publishes in it is one the caller can
//! > reach AS GIVEN: an ADDRESS LITERAL with the port the core assigned the socket that serves
//! > the api's plane, the endpoint the caller itself is configured with, or the api's own
//! > documented `{api-id}.execute-api.{region}.amazonaws.com` host, which AWS answers for and
//! > this emulator's front door claims. It is NEVER a name this emulator MINTS in a zone of
//! > its own: nothing on the machine this emulator runs on resolves such a name, so a client
//! > that honours the published value reaches nothing at all. Whatever the authority, the api
//! > is named in the address — in the host of its documented form, in the reserved data-plane
//! > path (`/_aws/execute-api/{api-id}`) of an address of this emulator — and the published
//! > value SERVES the api's route.
//!
//! The rows span what the rule generalises over: whether this emulator is served on a port at
//! all (which is what makes it open a socket for an api's data plane and publish that socket's
//! address), the form the served base is written in (an IPv4 literal, a loopback NAME, a
//! bracketed IPv6 literal), the endpoint the control-plane client is configured with (one of
//! this emulator's own addresses, and one of AWS's own service hosts, which is what a caller
//! inside the emulator — CloudFormation provisioning a stack's api — sends), the Region the
//! request is signed for, and the operation that MINTED the api (`CreateApi`, `ImportApi`).
//! The recorded case is ONE row: an api minted by a request that addressed this service's own
//! AWS host on an emulator served on a port.
//!
//! What this suite does not restate: the FORM of the value for each of those addresses
//! (`http_api_published_invoke_address` for an api a client of the emulator's own endpoint
//! created, `published_endpoint_documented_host` for one published on its plane's own socket),
//! and what the socket decides for a request arriving there
//! (`published_invoke_socket_front_door`).

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::api_lifecycle::{as_str, call_at_host, emulator};
use crate::websocket_handshake_data_plane::{send, Answer};

/// The status an `HTTP_PROXY` route answers: the upstream is a URI the CALLER can reach, so the
/// caller is sent there — `307`, with the integration's own `IntegrationUri` in `Location`.
/// Reaching it proves the request resolved all the way to the route's integration, which is
/// what a published address being SERVED means.
const SERVED: u16 = 307;
const UPSTREAM: &str = "https://upstream.example.com/items";

/// The front door's envelope for a request no service claimed. An address nothing serves is the
/// defect this suite pins, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The route every row deploys, and the path within the stage that matches it.
const ROUTE_KEY: &str = "GET /items";
const ROUTE_PATH: &str = "/items";

/// The reserved stage "that serves an api's root", reached with no stage segment at all.
const DEFAULT_STAGE: &str = "$default";

/// The zone RFC 6761 reserves for the loopback interface. A NAME written under it is what this
/// emulator used to mint for an api's endpoint, and no resolver on the machine this emulator
/// runs on answers for one, so no published authority may be such a name.
const RESERVED_ZONE_SUFFIX: &str = ".localhost";

/// Which operation MINTS a row's api.
#[derive(Clone, Copy)]
enum MintedBy {
    /// `CreateApi`, over a name and a protocol type.
    Create,
    /// `ImportApi`, over an OpenAPI 3 definition carrying the same route.
    Import,
}

/// One row: the base URL this emulator reports as its own (`None` for one served on no port at
/// all), the endpoint the control-plane client is configured with, the Region it signs for, and
/// the operation that mints the api.
struct Row {
    case: &'static str,
    served_base: Option<&'static str>,
    host: &'static str,
    region: &'static str,
    minted_by: MintedBy,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: an emulator served on a port, the api minted by a caller \
                   that addressed this service's own AWS host",
            served_base: Some("https://127.0.0.1:9297"),
            host: "apigateway.us-west-2.amazonaws.com",
            region: "us-west-2",
            minted_by: MintedBy::Create,
        },
        Row {
            case: "an emulator served on a loopback NAME and port, the api minted by a client \
                   of that endpoint",
            served_base: Some("https://localhost:34181"),
            host: "localhost:34181",
            region: "eu-west-1",
            minted_by: MintedBy::Create,
        },
        Row {
            case: "an emulator served on a bracketed IPv6 literal, the api minted by an import",
            served_base: Some("https://[::1]:9297"),
            host: "127.0.0.1:9297",
            region: "ap-southeast-2",
            minted_by: MintedBy::Import,
        },
        Row {
            case: "an emulator served on no port at all, the api minted by a client of its own \
                   endpoint",
            served_base: None,
            host: "127.0.0.1:40705",
            region: "eu-north-1",
            minted_by: MintedBy::Create,
        },
        Row {
            case: "an emulator served on no port at all, the api minted by a caller that \
                   addressed this service's own AWS host",
            served_base: None,
            host: "apigateway.us-east-1.amazonaws.com",
            region: "us-east-1",
            minted_by: MintedBy::Import,
        },
    ]
}

/// This emulator, served at the row's base URL when it names one.
fn emulator_for(row: &Row) -> MultiServiceRouter {
    let mut router = emulator();
    if let Some(base) = row.served_base {
        router.set_url_base(EndpointUrl::parse(base).unwrap_or_else(|| {
            panic!("setup: {base:?} must be a base URL this emulator can be served at")
        }));
    }
    router
}

/// The OpenAPI 3 definition an import row PUTs: the same route over the same upstream, so what
/// the row is asserted on is the address the import publishes.
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

/// The authority a client dials and the path it carries ahead of the stage — empty for a
/// published value that is a bare host.
fn published_parts(published: &str) -> (String, String) {
    let rest = published
        .split_once("://")
        .map_or(published, |(_scheme, rest)| rest);
    match rest.find('/') {
        Some(at) => (rest[..at].to_string(), rest[at..].to_string()),
        None => (rest.to_string(), String::new()),
    }
}

/// The host of an authority, without the port a published address carries and without the
/// brackets an IPv6 literal is written in.
fn host_of(authority: &str) -> &str {
    let host = match authority.starts_with('[') {
        true => authority
            .split_once(']')
            .map_or(authority, |(host, _port)| host),
        false => authority
            .rsplit_once(':')
            .map_or(authority, |(host, _port)| host),
    };
    host.trim_start_matches('[').trim_end_matches(']')
}

/// Assert `published` is an address the row's client can dial AS GIVEN, and answer the
/// authority and base path an invoke URL is built from.
///
/// The three addresses that are: an address LITERAL (the socket this emulator opened for the
/// api's plane, or the endpoint the client is configured with written as an address), the
/// endpoint the client is configured with exactly as it wrote it, and the api's own documented
/// `execute-api` host, which AWS answers for. A name this emulator minted under the zone
/// reserved for the loopback interface is none of them, whatever else it carries.
fn assert_dialable_as_given(row: &Row, api_id: &str, published: &str) -> (String, String) {
    let case = row.case;
    assert!(
        published.starts_with("https://"),
        "case {case:?}: an HTTP api is invoked over https, got {published:?}"
    );
    let (authority, base_path) = published_parts(published);
    let host = host_of(&authority).to_ascii_lowercase();
    let documented = format!("{api_id}.execute-api.{}.amazonaws.com", row.region);
    let dialable = host.parse::<std::net::IpAddr>().is_ok()
        || authority == row.host
        || host == documented;
    assert!(
        dialable,
        "case {case:?}: the published authority must be one the client can dial as given — an \
         address literal, the endpoint it is configured with ({:?}) or the api's own documented \
         host ({documented:?}) — got {published:?}",
        row.host
    );
    assert!(
        !host.ends_with(RESERVED_ZONE_SUFFIX),
        "case {case:?}: no published authority may be a name this emulator minted under the \
         zone reserved for the loopback interface, which nothing on this machine resolves, got \
         {published:?}"
    );
    // Whatever the authority, the value NAMES the api: in the host of the documented form, and
    // in the reserved data-plane path of an address of this emulator.
    let names_the_api = match host == documented {
        true => base_path.is_empty(),
        false => base_path == format!("/_aws/execute-api/{api_id}"),
    };
    assert!(
        names_the_api,
        "case {case:?}: the published value must name the api — in its host for the documented \
         form, in the reserved data-plane path for an address of this emulator — got \
         {published:?}"
    );
    (authority, base_path)
}

/// Mint the row's api and answer `(ApiId, the endpoint the minting operation published)`.
async fn minted(emulator: &MultiServiceRouter, row: &Row, name: &str) -> (String, String) {
    match row.minted_by {
        MintedBy::Create => {
            let (status, created) = call_at_host(
                emulator,
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
            (as_str(&created, "apiId"), as_str(&created, "apiEndpoint"))
        }
        MintedBy::Import => {
            let (status, imported) = call_at_host(
                emulator,
                row.host,
                row.region,
                "PUT",
                "/v2/apis",
                Some(json!({ "body": definition(name) })),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: ImportApi must answer 200, got {status} {imported}",
                row.case
            );
            (as_str(&imported, "apiId"), as_str(&imported, "apiEndpoint"))
        }
    }
}

/// Give a CREATED api the route an import's definition already carried.
async fn given_route(emulator: &MultiServiceRouter, row: &Row, api_id: &str) {
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
}

/// Release the api's `$default` stage, which is what makes a route of it servable.
async fn given_stage(emulator: &MultiServiceRouter, row: &Row, api_id: &str) {
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

/// RULE: every address an HTTP api publishes as its `ApiEndpoint` is one the client that was
/// handed it can dial as given — never a name this emulator minted in a zone of its own — it
/// names the api, every reporter of the api repeats it, and the api's route is SERVED there.
#[tokio::test]
async fn a_published_endpoint_is_dialable_as_given_and_serves_the_apis_route() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // One emulator per row: the row's own served base decides the address an api of it is
        // published on.
        let emulator = emulator_for(&row);
        let name = format!("dialable-endpoint-{index}");

        let (api_id, published) = minted(&emulator, &row, &name).await;
        if matches!(row.minted_by, MintedBy::Create) {
            given_route(&emulator, &row, &api_id).await;
        }
        given_stage(&emulator, &row, &api_id).await;

        let (authority, base_path) = assert_dialable_as_given(&row, &api_id, &published);

        // The value is the api's own, settled when it was minted, so every reporter repeats it.
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
            "case {case:?}: GetApi must answer 200, got {status} {fetched}"
        );
        assert_eq!(
            as_str(&fetched, "apiEndpoint"),
            published,
            "case {case:?}: GetApi must report the published endpoint, got {fetched}"
        );
        let (status, listed) =
            call_at_host(&emulator, row.host, row.region, "GET", "/v2/apis", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetApis must answer 200, got {status} {listed}"
        );
        let listed_endpoint = listed
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("apiId").and_then(Value::as_str) == Some(&api_id))
            })
            .map(|item| as_str(item, "apiEndpoint"))
            .unwrap_or_default();
        assert_eq!(
            listed_endpoint, published,
            "case {case:?}: GetApis must list the published endpoint, got {listed}"
        );

        // And the published value SERVES the api: the invoke URL a client builds from it — the
        // value itself for the stage that serves the api's root, with the route path appended —
        // reaches that route's own integration.
        let invoke_path = format!("{base_path}{ROUTE_PATH}");
        let answered: Answer = send(&emulator, &authority, "GET", &invoke_path, &[]).await;
        assert_ne!(
            answered.error_type.as_deref(),
            Some(UNKNOWN_SERVICE),
            "case {case:?}: the published address must be one this emulator claims, but \
             {authority}{invoke_path} is claimed by no service: {}",
            answered.summary()
        );
        assert_eq!(
            answered.status,
            SERVED,
            "case {case:?}: {authority}{invoke_path} must reach the route's integration, got {}",
            answered.summary()
        );
        assert_eq!(
            answered.location.as_deref(),
            Some(UPSTREAM),
            "case {case:?}: {authority}{invoke_path} must be sent to the upstream the \
             integration names, got {}",
            answered.summary()
        );
    }
}
