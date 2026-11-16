// SPDX-License-Identifier: CC0-1.0

//! The PATH an HTTP api stage's access-log record reports for a request it served.
//!
//! The rule every row below holds the data plane to: **`$context.path` in an access-log
//! record is the path the api's OWN endpoint was addressed at — the stage's name as the first
//! segment for a NAMED stage, because that stage's invoke URL carries it ("the stage name is
//! typically appended to this URI to form a complete path to a deployed API stage",
//! `Api$ApiEndpoint`), and no stage segment at all for the `$default` stage, which serves the
//! api's root — whichever of the addressings a client on this machine reached the emulator
//! by. It is the path ASKED FOR, so the route TEMPLATE the request matched belongs to
//! `$context.routeKey` and not here. `$context.domainName` is that same endpoint's HOST, for
//! the same reason.**
//!
//! It is the same reading the proxy event's own path members take
//! ([`crate::http_api_event_path`]), over the same addressings: a client cannot resolve
//! `{api-id}.execute-api.{region}.amazonaws.com` on the machine this emulator runs on, so it
//! carries in the PATH what it cannot carry in the host — the api id ahead of the stage, the
//! local endpoint marker, or the stage's own name — and none of that is part of the path the
//! api was addressed at. A record that reported one would hand a client a path no AWS
//! deployment produces, so a per-endpoint dashboard, a metric filter keyed on a path prefix
//! or any attribution of a record to an endpoint would match here what it does not match at
//! AWS, and records captured against this emulator would not compare with records captured
//! against AWS.
//!
//! The rows differ along what the rule generalises over: HOW the request addresses the api
//! (the api's own `execute-api` host, that host with the root stage's name spelled out, the
//! local endpoint marker with and without that name, the marker carrying the endpoint HOST
//! instead of the api id, the api id as the first path segment, and the stage segment alone),
//! which stage serves it (the one that serves the api's root and two named ones), the SHAPE of
//! the route key the path matches (a literal path, a path variable, the greedy remainder and
//! the `$default` catch-all), and the number of path segments, down to the api's own root.
//!
//! Every assertion reads the record back through CloudWatch Logs' own reader
//! (`FilterLogEvents`), so a record that did not reach that store cannot pass.

use serde_json::Value;

use crate::http_api_access_logging::{create_log_group, delivered_to, emulator, log_group_arn};
use crate::http_api_backend_dispatch::{given_api, given_function, lambda_integration};
use crate::websocket_handshake_data_plane::send;

/// The stage that serves an api's root, reached with no stage segment in the path at all.
const ROOT_STAGE: &str = "$default";

/// The marker a rewritten endpoint keeps the api's own endpoint behind, as
/// `service::data_plane::LOCAL_ENDPOINT_PREFIX` spells it.
const LOCAL_MARKER: &str = "/_aws/execute-api";

/// The marker WITHOUT the `execute-api` label: the spelling a rewriter that carried the
/// endpoint's own HOST across produces, which the data plane reads on any host too.
const LOCAL_MARKER_PREFIX: &str = "/_aws";

/// An address of the emulator's own endpoint, which names no service: what a client that
/// cannot resolve the api's host sends to.
const LOCAL_HOST: &str = "127.0.0.1:9297";

/// The destination every row points its stage at.
const GROUP: &str = "/apigwv2/access-path";

/// The function behind every route, and the payload format its integration carries.
const FUNCTION: &str = "access-log-path-function";
const PAYLOAD_FORMAT: &str = "2.0";

/// The handler behind the route: it answers the structured response the `2.0` format defines.
const HANDLER_SOURCE: &str = "def lambda_handler(event, context):\n    \
                              return {'statusCode': 200, 'body': 'served'}\n";

/// The format every stage below renders its records by: the request's own id, the route key it
/// matched, the path it asked for and the endpoint host it asked at.
const FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","routeKey":"$context.routeKey","#,
    r#""path":"$context.path","domainName":"$context.domainName"}"#
);

/// The header API Gateway reports the id of the request it processed in, which the record's
/// `$context.requestId` has to equal.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How the request reaches the emulator — the addressings `service::invoke` resolves, as
/// [`crate::http_api_event_path`] names them.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// The api's own `ApiEndpoint` host: the invoke URL path and nothing else.
    ApiHost,
    /// The api's own host with the stage's name spelled out as the first segment.
    ApiHostNamingTheStage,
    /// The emulator's own endpoint, with the api's endpoint behind the local marker.
    LocalMarker,
    /// The same, with the stage's name spelled out too.
    LocalMarkerNamingTheStage,
    /// The same, with the api's endpoint HOST behind the marker in place of its id — the other
    /// spelling a rewriter of that endpoint produces.
    LocalMarkerCarryingTheHost,
    /// The emulator's own endpoint, with the api id as the first path segment.
    ApiIdFirst,
    /// The emulator's own endpoint, with the stage segment alone naming the deployment.
    StageOnly,
}

/// One row: the stage that serves the request, the route the api declares, the path within
/// that stage the request asks for, and the addressing it arrives by.
struct Row {
    case: &'static str,
    stage: &'static str,
    route_key: &'static str,
    /// The path within the stage — what the route key matches.
    route_path: &'static str,
    addressing: Addressing,
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
        Addressing::LocalMarkerCarryingTheHost => (
            LOCAL_HOST.to_string(),
            format!("{LOCAL_MARKER_PREFIX}/{endpoint_host}{aws_path}"),
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
            route_key: "GET /items/{item}",
            route_path: "/items/one",
            addressing: Addressing::ApiHost,
        },
        Row {
            case: "the root stage behind the local endpoint marker",
            stage: ROOT_STAGE,
            route_key: "GET /items/{item}",
            route_path: "/items/one",
            addressing: Addressing::LocalMarker,
        },
        Row {
            case: "the root stage behind the marker, with its own name too",
            stage: ROOT_STAGE,
            route_key: "GET /items/{item}",
            route_path: "/items/one",
            addressing: Addressing::LocalMarkerNamingTheStage,
        },
        Row {
            case: "the root stage behind the marker carrying the endpoint host",
            stage: ROOT_STAGE,
            route_key: "GET /orders/{id}/items",
            route_path: "/orders/7/items",
            addressing: Addressing::LocalMarkerCarryingTheHost,
        },
        Row {
            case: "the root stage with the api id as the first segment",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::ApiIdFirst,
        },
        Row {
            case: "the root stage named by its segment alone",
            stage: ROOT_STAGE,
            route_key: "GET /alpha",
            route_path: "/alpha",
            addressing: Addressing::StageOnly,
        },
        Row {
            case: "the root stage reached with its own name as a path segment",
            stage: ROOT_STAGE,
            route_key: "ANY /files/{proxy+}",
            route_path: "/files/a/b/c",
            addressing: Addressing::ApiHostNamingTheStage,
        },
        Row {
            case: "the root stage at the api's own root path, matched by the catch-all route",
            stage: ROOT_STAGE,
            route_key: "$default",
            route_path: "/",
            addressing: Addressing::ApiHost,
        },
        Row {
            case: "a named stage on the api's own host",
            stage: "prod",
            route_key: "GET /orders/{id}/items",
            route_path: "/orders/7/items",
            addressing: Addressing::ApiHost,
        },
        Row {
            case: "a named stage behind the local endpoint marker",
            stage: "prod",
            route_key: "GET /items/{item}",
            route_path: "/items/one",
            addressing: Addressing::LocalMarker,
        },
        Row {
            case: "a named stage behind the marker carrying the endpoint host, the catch-all \
                   route",
            stage: "prod",
            route_key: "$default",
            route_path: "/anything/at/all",
            addressing: Addressing::LocalMarkerCarryingTheHost,
        },
        Row {
            case: "a named stage named by its segment alone",
            stage: "staging",
            route_key: "ANY /files/{proxy+}",
            route_path: "/files/a/b",
            addressing: Addressing::StageOnly,
        },
    ]
}

/// The record a stage delivered for the request it just served: the one line in the
/// destination group, parsed as the JSON the format is.
async fn record(router: &smithy_server_runtime::MultiServiceRouter, case: &str) -> Value {
    let records = delivered_to(router, GROUP)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: no destination group {GROUP:?} at all"));
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected one record for the one request served, got {records:?}"
    );
    let line = records
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: no record in {GROUP:?}"));
    serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("case {case:?}: the record {line:?} is not the JSON the format is: {error}")
    })
}

/// Whichever addressing a client reached the emulator by, the record reports the path the
/// api's own endpoint was addressed at — and the route template only as the route key.
#[tokio::test]
async fn a_record_reports_the_path_the_apis_own_endpoint_was_addressed_at() {
    for row in rows() {
        let case = row.case;
        // One api per row, in a store of its own: two of the addressings name no api at all
        // and are resolved by the STAGE segment, which only names one deployment while one
        // api holds a stage of that name.
        let (router, engine) = emulator();
        create_log_group(&router, GROUP).await;
        let function = given_function(&router, &engine, case, FUNCTION, HANDLER_SOURCE).await;
        let (api_id, endpoint_host) = given_api(
            &router,
            case,
            lambda_integration(&function, PAYLOAD_FORMAT),
            row.route_key,
            serde_json::json!({
                "stageName": row.stage,
                "autoDeploy": true,
                "accessLogSettings": {
                    "destinationArn": log_group_arn(GROUP),
                    "format": FORMAT,
                },
            }),
        )
        .await;
        let (host, path) = addressed(&row, &api_id, &endpoint_host);
        let answered = send(&router, &host, "GET", &path, &[]).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected the function's 200 for {path:?}, got {}",
            answered.summary()
        );
        let request_id = answered
            .header(REQUEST_ID_HEADER)
            .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));

        let rendered = record(&router, case).await;
        let expected = endpoint_path(row.stage, row.route_path);
        assert_eq!(
            rendered,
            serde_json::json!({
                "requestId": request_id,
                "routeKey": row.route_key,
                "path": expected,
                "domainName": endpoint_host,
            }),
            "case {case:?}: expected the record of {path:?} to report the path {expected:?} \
             asked for at the endpoint {endpoint_host:?}, got {rendered}"
        );
        // Whatever the addressing carried, the path the record reports names neither the
        // emulator's own marker, nor the api id, nor the root stage's reserved name — nor the
        // template of the route key that matched it.
        let reported = rendered
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !reported.contains(LOCAL_MARKER_PREFIX)
                && !reported.contains(&api_id)
                && !reported.contains(ROOT_STAGE)
                && !reported.contains('{'),
            "case {case:?}: expected the record's path to carry neither the local marker, the \
             api id, the root stage's name nor a route template, got {reported:?}"
        );
    }
}
