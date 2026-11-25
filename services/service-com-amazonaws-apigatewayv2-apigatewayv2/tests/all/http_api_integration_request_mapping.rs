// SPDX-License-Identifier: CC0-1.0

//! What an HTTP api's integration `RequestParameters` do to the request the BACKEND receives,
//! driven end to end through the `execute-api` data plane.
//!
//! The rule every row below is one slice of:
//!
//! > **For an HTTP api integration that names no `IntegrationSubtype`, each entry of its
//! > `RequestParameters` is one operation on the request the backend is sent — "a key-value map
//! > specifying how to transform HTTP requests before sending them to the backend. The key
//! > should follow the pattern `<action>:<header|querystring|path>.<location>` where action can
//! > be append, overwrite or remove"
//! > (`smithy_query(query="doc",shape="CreateIntegrationRequest",member="RequestParameters")`)
//! > — and the gateway APPLIES every one of them before the backend receives the request:
//! > `overwrite` leaves that one header or query parameter holding the mapped value alone,
//! > `append` adds the mapped value beside the ones the client sent, `remove` strips exactly
//! > that one named field and leaves every other one the client sent intact, and
//! > `overwrite:path` addresses the backend at the mapped path. A value is a static one or a
//! > mapping expression (`$request.…`, `$context.…`, `$stageVariables.…`) resolved against the
//! > call, and one naming something the call does not carry maps nothing. An integration
//! > carrying no `RequestParameters` forwards the request exactly as it arrived.**
//!
//! The rows span what the rule generalises over: each action over each operand the key grammar
//! names, a field the client sent and one it did not, the sources a value is mapped from
//! (a `$context` variable, a `$request` part, a static value, an expression the call has no
//! value for) and the UNMAPPED twin route over the same backend, which shows none of the
//! effects in the same instant.
//!
//! Nothing here is a value this emulator fills in: every assertion is on the request a
//! function RAN and echoed back, which is why AWS Lambda is mounted beside this service on one
//! engine. `src/service/invoke/request_parameters.rs` holds the same rule's unit table over
//! the grammar; this suite is what says the mapping reaches the backend at all.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    control, emulator_with_lambda, given_api, given_function, lambda_integration,
};
use crate::websocket_handshake_data_plane::{send, Answer};

/// The statuses the gateway answers while a backend's runtime is still starting, which the
/// wait below looks past ([`answered`]).
const INTEGRATION_FAULT: u16 = 500;
const TIMED_OUT: u16 = 503;

/// How long a request keeps being made while the origin's function is still starting, and how
/// often: generous, and bounding no behaviour any row asserts on. Nothing here measures how
/// long anything took.
const ANSWER_WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The headers the client sends on every row: one the mappings operate on, and one no mapping
/// names, which must reach the backend untouched whatever the row maps.
const CLIENT_HEADER: &str = "x-sim2real-client";
const CLIENT_HEADER_VALUE: &str = "sent-by-the-client";
const KEPT_HEADER: &str = "x-sim2real-keep";
const KEPT_HEADER_VALUE: &str = "kept";

/// The query string the client sends on every row: one parameter the mappings operate on and
/// one they do not.
const CLIENT_QUERY: &str = "page=1&drop=yes";

/// The handler behind the origin api's route: it echoes the request it received — the headers
/// (lower-cased, as a `2.0` proxy event states them), the raw path and the raw query string.
const ECHO_RECEIVED_REQUEST: &str = r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "headers": event.get("headers") or {},
                "rawPath": event.get("rawPath"),
                "rawQueryString": event.get("rawQueryString"),
            }
        ),
    }
"#;

/// What one row expects the backend to read for one header.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// The header reaches the backend with exactly this value.
    Exactly(&'static str),
    /// The backend reads no such header at all.
    Absent,
    /// The backend reads a value the CLIENT never sent and that no row states: a value the
    /// gateway itself minted for this call, which is a per-run value — so what is asserted is
    /// that it is there, that it is long enough to be an id, and that it is none of the
    /// client's own.
    MintedByTheGateway,
}

/// One row: the front route, the mapping its integration carries, and what the backend must
/// then read.
struct Row {
    case: &'static str,
    /// The path within the front api that reaches this row's route.
    path: &'static str,
    /// The row's `RequestParameters`, as `CreateIntegration` takes them.
    parameters: Option<Value>,
    /// What the backend must read for each header the row is about.
    headers: &'static [(&'static str, Expected)],
    /// The query-string parameters the backend must read, by name, each with the ONE value it
    /// must hold — or `None` for a parameter it must read none of.
    query: &'static [(&'static str, Option<&'static str>)],
    /// The path the backend must be addressed at.
    raw_path: &'static str,
}

/// The path the origin's upstream names, which every row but the path-mapping one reaches.
const ORIGIN_PATH: &str = "/echo";

/// The path one row maps the request onto — another path of the origin api's own greedy route.
const MAPPED_PATH: &str = "/elsewhere";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the unmapped twin route over the same backend",
            path: "/plain",
            parameters: None,
            headers: &[
                (CLIENT_HEADER, Expected::Exactly(CLIENT_HEADER_VALUE)),
                (KEPT_HEADER, Expected::Exactly(KEPT_HEADER_VALUE)),
                ("x-sim2real-injected", Expected::Absent),
            ],
            query: &[("page", Some("1")), ("drop", Some("yes")), ("injected", None)],
            raw_path: ORIGIN_PATH,
        },
        Row {
            case: "the recorded mapping: a header injected from a context variable, one \
                   removed, and a query parameter appended",
            path: "/mapped",
            parameters: Some(json!({
                "overwrite:header.x-sim2real-injected": "$context.requestId",
                "remove:header.x-sim2real-client": "''",
                "append:querystring.injected": "static-q"
            })),
            headers: &[
                (CLIENT_HEADER, Expected::Absent),
                (KEPT_HEADER, Expected::Exactly(KEPT_HEADER_VALUE)),
                ("x-sim2real-injected", Expected::MintedByTheGateway),
            ],
            query: &[
                ("page", Some("1")),
                ("drop", Some("yes")),
                ("injected", Some("static-q")),
            ],
            raw_path: ORIGIN_PATH,
        },
        Row {
            case: "an overwrite and an append of headers the client DID send, with a query \
                   parameter overwritten and another removed",
            path: "/rewritten",
            parameters: Some(json!({
                "overwrite:header.x-sim2real-keep": "mapped-static",
                "append:header.x-sim2real-client": "appended",
                "overwrite:querystring.page": "9",
                "remove:querystring.drop": "''"
            })),
            headers: &[
                (
                    CLIENT_HEADER,
                    // An appended value reaches the backend beside the one the client sent,
                    // which a `2.0` event states as the occurrences joined.
                    Expected::Exactly("sent-by-the-client,appended"),
                ),
                (KEPT_HEADER, Expected::Exactly("mapped-static")),
                ("x-sim2real-injected", Expected::Absent),
            ],
            query: &[("page", Some("9")), ("drop", None), ("injected", None)],
            raw_path: ORIGIN_PATH,
        },
        Row {
            case: "a header mapped from a part of the request, and the path overwritten",
            path: "/moved",
            parameters: Some(json!({
                "overwrite:header.x-sim2real-injected": "$request.header.x-sim2real-keep",
                "overwrite:path": MAPPED_PATH
            })),
            headers: &[
                (CLIENT_HEADER, Expected::Exactly(CLIENT_HEADER_VALUE)),
                (KEPT_HEADER, Expected::Exactly(KEPT_HEADER_VALUE)),
                ("x-sim2real-injected", Expected::Exactly(KEPT_HEADER_VALUE)),
            ],
            query: &[("page", Some("1")), ("drop", Some("yes")), ("injected", None)],
            raw_path: MAPPED_PATH,
        },
        Row {
            case: "a mapping whose source this call carries no value for, which maps nothing",
            path: "/absent",
            parameters: Some(json!({
                "overwrite:header.x-sim2real-injected": "$request.header.x-sim2real-absent",
                "append:querystring.injected": "$request.querystring.absent"
            })),
            headers: &[
                (CLIENT_HEADER, Expected::Exactly(CLIENT_HEADER_VALUE)),
                (KEPT_HEADER, Expected::Exactly(KEPT_HEADER_VALUE)),
                ("x-sim2real-injected", Expected::Absent),
            ],
            query: &[("page", Some("1")), ("drop", Some("yes")), ("injected", None)],
            raw_path: ORIGIN_PATH,
        },
    ]
}

/// The ORIGIN api: one `AWS_PROXY` route running [`ECHO_RECEIVED_REQUEST`] over every path of
/// the stage that serves its root. Answers the upstream a front integration fronts it at.
async fn given_origin_api(
    emulator: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
) -> String {
    let arn = given_function(emulator, engine, case, "mapping-origin", ECHO_RECEIVED_REQUEST).await;
    let (_, host) = given_api(
        emulator,
        case,
        lambda_integration(&arn, "2.0"),
        "ANY /{proxy+}",
        json!({ "stageName": "$default", "autoDeploy": true }),
    )
    .await;
    format!("https://{host}{ORIGIN_PATH}")
}

/// The FRONT api: one `HTTP_PROXY` route per row over `upstream`, each with that row's own
/// `RequestParameters`, and the stage that serves the api's root. Answers the host its
/// `ApiEndpoint` names.
///
/// Every integration is created before the stage, so one deployment serves them all.
async fn given_front_api(emulator: &MultiServiceRouter, upstream: &str, rows: &[Row]) -> String {
    let case = "the front api of the request-mapping table";
    let (status, api) = control(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "front-request-mapping", "protocolType": "HTTP" })),
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
    for row in rows {
        let case = row.case;
        let mut integration = json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": upstream,
            "integrationMethod": "ANY",
            "payloadFormatVersion": "1.0"
        });
        if let Some(parameters) = row.parameters.clone() {
            integration["requestParameters"] = parameters;
        }
        let (status, created) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/integrations"),
            Some(integration),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateIntegration answered {status} {created}"
        );
        let integration_id = created
            .get("integrationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // The mapping is persisted as authored, which is what the data plane then evaluates.
        if let Some(parameters) = row.parameters.as_ref() {
            assert_eq!(
                created.get("requestParameters"),
                Some(parameters),
                "case {case:?}: CreateIntegration must report the mapping it was given, got \
                 {created}"
            );
        }
        let (status, route) = control(
            emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": format!("ANY {}", row.path),
                "target": format!("integrations/{integration_id}")
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateRoute answered {status} {route}"
        );
    }
    let (status, stage) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": "$default", "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage answered {status} {stage}"
    );
    host
}

/// One request, asked again until the origin's own answer arrives or [`ANSWER_WAIT`] passes:
/// the FIRST invocation of a function starts its runtime, which the gateway answers its own
/// [`INTEGRATION_FAULT`] (or [`TIMED_OUT`]) for while it is starting. The wait is for the
/// answer — a condition — and nothing here asserts on the clock.
async fn answered(emulator: &MultiServiceRouter, case: &str, host: &str, path: &str) -> Answer {
    let deadline = Instant::now() + ANSWER_WAIT;
    loop {
        let answer = send(
            emulator,
            host,
            "GET",
            &format!("{path}?{CLIENT_QUERY}"),
            &[
                (CLIENT_HEADER, CLIENT_HEADER_VALUE),
                (KEPT_HEADER, KEPT_HEADER_VALUE),
            ],
        )
        .await;
        if answer.status == 200 {
            return answer;
        }
        assert!(
            matches!(answer.status, INTEGRATION_FAULT | TIMED_OUT) && Instant::now() < deadline,
            "case {case:?}: expected the origin's own answer, got {}",
            answer.summary()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What the origin echoed: the value it read for the header `name`, or `None` for one it read
/// no value for.
fn header_read(answer: &Answer, name: &str) -> Option<String> {
    let echoed: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
    echoed
        .pointer(&format!("/headers/{name}"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A member of what the origin echoed about the request it received.
fn echoed(answer: &Answer, member: &str) -> String {
    let echoed: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
    echoed
        .pointer(&format!("/{member}"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Every value the raw query string the origin read states for `name`.
fn query_values(answer: &Answer, name: &str) -> Vec<String> {
    echoed(answer, "rawQueryString")
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .filter(|(held, _)| *held == name)
        .map(|(_, value)| value.to_string())
        .collect()
}

/// How many characters a value the gateway minted for this call must carry at least, so a
/// value that is there but empty (or a single character) is not mistaken for an id: the
/// request ids this gateway mints are far longer. The VALUE itself is per-run and is never
/// asserted on.
const MINTED_AT_LEAST: usize = 8;

#[tokio::test]
async fn an_integrations_request_parameters_transform_the_request_the_backend_receives() {
    let case = "an HTTP_PROXY route over an origin api that echoes what it received";
    let (emulator, engine) = emulator_with_lambda();
    let upstream = given_origin_api(&emulator, &engine, case).await;
    let rows = rows();
    let front = given_front_api(&emulator, &upstream, &rows).await;

    for row in &rows {
        let case = row.case;
        let answer = answered(&emulator, case, &front, row.path).await;

        for (name, expected) in row.headers {
            let read = header_read(&answer, name);
            match expected {
                Expected::Exactly(value) => assert_eq!(
                    read.as_deref(),
                    Some(*value),
                    "case {case:?}: expected the backend to read {name} as {value:?}, got \
                     {read:?}"
                ),
                Expected::Absent => assert_eq!(
                    read, None,
                    "case {case:?}: expected the backend to read no {name} at all, got {read:?}"
                ),
                Expected::MintedByTheGateway => {
                    let read = read.unwrap_or_default();
                    assert!(
                        read.len() >= MINTED_AT_LEAST,
                        "case {case:?}: expected the backend to read {name} as a value the \
                         gateway minted for this call, got {read:?}"
                    );
                    assert_ne!(
                        read, CLIENT_HEADER_VALUE,
                        "case {case:?}: expected {name} to carry none of the client's own \
                         values, got {read:?}"
                    );
                    assert_ne!(
                        read, KEPT_HEADER_VALUE,
                        "case {case:?}: expected {name} to carry none of the client's own \
                         values, got {read:?}"
                    );
                }
            }
        }

        for (name, expected) in row.query {
            let read = query_values(&answer, name);
            match expected {
                Some(value) => assert_eq!(
                    read,
                    vec![(*value).to_string()],
                    "case {case:?}: expected the backend to read the parameter {name} as \
                     {value:?}, got {read:?} (query {:?})",
                    echoed(&answer, "rawQueryString")
                ),
                None => assert!(
                    read.is_empty(),
                    "case {case:?}: expected the backend to read no parameter {name}, got \
                     {read:?}"
                ),
            }
        }

        let path = echoed(&answer, "rawPath");
        assert_eq!(
            path, row.raw_path,
            "case {case:?}: expected the backend addressed at {:?}, got {path:?}",
            row.raw_path
        );
    }
}
