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
//!
//! The second test holds the SAME rule over every consumer of that path at once: the request
//! is admitted and normalised ONCE, so the record's `$context.path` and the path members of
//! the proxy event the backend is handed report the same value for the one request that
//! produced both — as do the other values each derives from the front door (the endpoint host,
//! the verb, the serving stage and the id of the request). A client joins a record to a
//! request by exactly those values, so a consumer that read the socket-level target where
//! another read the api-relative path would make that join match here what it does not match
//! at AWS. Its event half is read off what the FUNCTION returned, and its rows span the
//! payload format versions as well, whose path members are spelled differently.

use serde_json::Value;

use crate::http_api_access_logging::{create_log_group, delivered_to, emulator, log_group_arn};
use crate::http_api_backend_dispatch::{
    given_api, given_function, lambda_integration, ECHO_EVENT,
};
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

/// The record a stage delivered for the request it just served: the one line in `group`,
/// parsed as the JSON the format is.
async fn record_in(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
    case: &str,
) -> Value {
    let records = delivered_to(router, group)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: no destination group {group:?} at all"));
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected one record for the one request served, got {records:?}"
    );
    let line = records
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: no record in {group:?}"));
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

        let rendered = record_in(&router, GROUP, case).await;
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

/// The destination the agreement rows below point their stages at: their own group, so the one
/// record a row delivers is the only line in it.
const AGREEMENT_GROUP: &str = "/apigwv2/access-path-agreement";

/// The format the agreement rows render their records by: the four values an event states too,
/// each under the member name the record carries it in.
const AGREEMENT_FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","path":"$context.path","#,
    r#""domainName":"$context.domainName","httpMethod":"$context.httpMethod","#,
    r#""stage":"$context.stage"}"#
);

/// Where a payload format of the proxy event states each value the record above also reports,
/// as `(the record's member, the event's JSON pointer)`: that format's two path members, the
/// endpoint host, the verb, the serving stage and the request's own id.
///
/// The PATH members are the ones [`crate::http_api_event_path::path_members`] already owns,
/// read through that helper so the two suites cannot disagree about their spelling. The rest
/// are stated here: the `1.0` event keeps the verb on the request context itself (beside the
/// copy on the event) while `2.0` keeps it under `http`.
fn event_members(payload_format: &str) -> Vec<(&'static str, &'static str)> {
    let mut members: Vec<(&'static str, &'static str)> = vec![
        ("requestId", "/requestContext/requestId"),
        ("domainName", "/requestContext/domainName"),
        ("stage", "/requestContext/stage"),
    ];
    members.extend(
        crate::http_api_event_path::path_members(payload_format)
            .into_iter()
            .map(|(_described, pointer)| ("path", pointer)),
    );
    if payload_format == "2.0" {
        members.push(("httpMethod", "/requestContext/http/method"));
    } else {
        members.push(("httpMethod", "/httpMethod"));
        members.push(("httpMethod", "/requestContext/httpMethod"));
    }
    members
}

/// The rows the agreement test spans, each with the verb it is asked with and the payload
/// format version its integration builds the event in: the same dimensions as the rows above —
/// the addressing, the stage, the shape of the route key and the number of path segments — plus
/// the two payload formats and a verb per row.
fn agreement_rows() -> Vec<(Row, &'static str, &'static str)> {
    vec![
        (
            Row {
                case: "the root stage on the api's own host, a path variable",
                stage: ROOT_STAGE,
                route_key: "GET /items/{item}",
                route_path: "/items/one",
                addressing: Addressing::ApiHost,
            },
            "GET",
            "2.0",
        ),
        (
            Row {
                case: "the root stage behind the local endpoint marker, a path variable",
                stage: ROOT_STAGE,
                route_key: "GET /items/{item}",
                route_path: "/items/one",
                addressing: Addressing::LocalMarker,
            },
            "GET",
            "2.0",
        ),
        (
            Row {
                case: "the root stage behind the marker, with its own name too, under 1.0",
                stage: ROOT_STAGE,
                route_key: "GET /items/{item}",
                route_path: "/items/one",
                addressing: Addressing::LocalMarkerNamingTheStage,
            },
            "GET",
            "1.0",
        ),
        (
            Row {
                case: "the root stage behind the marker carrying the endpoint host, several \
                       segments",
                stage: ROOT_STAGE,
                route_key: "GET /orders/{id}/items",
                route_path: "/orders/7/items",
                addressing: Addressing::LocalMarkerCarryingTheHost,
            },
            "GET",
            "2.0",
        ),
        (
            Row {
                case: "the root stage with the api id as the first segment, the greedy remainder",
                stage: ROOT_STAGE,
                route_key: "ANY /files/{proxy+}",
                route_path: "/files/a/b/c",
                addressing: Addressing::ApiIdFirst,
            },
            "DELETE",
            "2.0",
        ),
        (
            Row {
                case: "the root stage at the api's own root path, the catch-all route, under 1.0",
                stage: ROOT_STAGE,
                route_key: "$default",
                route_path: "/",
                addressing: Addressing::ApiHostNamingTheStage,
            },
            "GET",
            "1.0",
        ),
        (
            Row {
                case: "a named stage on the api's own host, a literal path",
                stage: "prod",
                route_key: "GET /alpha",
                route_path: "/alpha",
                addressing: Addressing::ApiHost,
            },
            "GET",
            "2.0",
        ),
        (
            Row {
                case: "a named stage behind the local endpoint marker, the catch-all route",
                stage: "prod",
                route_key: "$default",
                route_path: "/anything/at/all",
                addressing: Addressing::LocalMarker,
            },
            "POST",
            "2.0",
        ),
        (
            Row {
                case: "a named stage named by its segment alone, the greedy remainder, under 1.0",
                stage: "staging",
                route_key: "ANY /files/{proxy+}",
                route_path: "/files/a/b",
                addressing: Addressing::StageOnly,
            },
            "GET",
            "1.0",
        ),
    ]
}

/// One request is admitted once: the record the stage delivered and the event the function was
/// handed report the SAME path — and the same endpoint host, verb, stage and request id.
#[tokio::test]
async fn the_record_and_the_event_report_one_admitted_request_path() {
    for (row, method, payload_format) in agreement_rows() {
        let case = row.case;
        // One api per row, in a store of its own, for the reason the suite above states.
        let (router, engine) = emulator();
        create_log_group(&router, AGREEMENT_GROUP).await;
        let function = given_function(&router, &engine, case, FUNCTION, ECHO_EVENT).await;
        let (api_id, endpoint_host) = given_api(
            &router,
            case,
            lambda_integration(&function, payload_format),
            row.route_key,
            serde_json::json!({
                "stageName": row.stage,
                "autoDeploy": true,
                "accessLogSettings": {
                    "destinationArn": log_group_arn(AGREEMENT_GROUP),
                    "format": AGREEMENT_FORMAT,
                },
            }),
        )
        .await;

        let (host, path) = addressed(&row, &api_id, &endpoint_host);
        let answered = send(&router, &host, method, &path, &[]).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected the function's 200 for {method} {path:?}, got {}",
            answered.summary()
        );
        let request_id = answered
            .header(REQUEST_ID_HEADER)
            .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));
        let event: Value = serde_json::from_str(&answered.text).unwrap_or(Value::Null);

        let rendered = record_in(&router, AGREEMENT_GROUP, case).await;
        let expected = endpoint_path(row.stage, row.route_path);
        assert_eq!(
            rendered,
            serde_json::json!({
                "requestId": request_id,
                "path": expected,
                "domainName": endpoint_host,
                "httpMethod": method,
                "stage": row.stage,
            }),
            "case {case:?}: expected the record of {method} {path:?} to report the path \
             {expected:?} asked for at the endpoint {endpoint_host:?}, got {rendered}"
        );
        for (member, pointer) in event_members(payload_format) {
            let stated = event.pointer(pointer).cloned().unwrap_or(Value::Null);
            let recorded = rendered.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                stated, recorded,
                "case {case:?}: expected {pointer} of the event for {method} {path:?} to agree \
                 with the record's {member:?} ({recorded}), got {stated}"
            );
        }
        // Neither consumer reports what the addressing carried: every path either of them
        // states is a path, and names neither the emulator's own marker, the api id, the root
        // stage's reserved name nor the template of the route key that matched.
        let mut reported: Vec<(String, String)> = Vec::new();
        for (member, pointer) in event_members(payload_format) {
            if member == "path" {
                reported.push((
                    pointer.to_string(),
                    event
                        .pointer(pointer)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ));
            }
        }
        reported.push((
            "the record's path".to_string(),
            rendered
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ));
        for (reader, actual) in reported {
            assert!(
                actual.starts_with('/')
                    && !actual.contains(LOCAL_MARKER_PREFIX)
                    && !actual.contains(&api_id)
                    && !actual.contains(ROOT_STAGE)
                    && !actual.contains('{'),
                "case {case:?}: expected {reader} for {method} {path:?} to be a path carrying \
                 neither the local marker, the api id, the root stage's name nor a route \
                 template, got {actual:?}"
            );
        }
    }
}
