// SPDX-License-Identifier: CC0-1.0

//! What the `execute-api` endpoint space answers at the ROOT of an endpoint whose api id
//! resolves to no api — the address a client keeps calling after the api behind it is gone.
//!
//! The rule these cases assert, one level above the recorded case (a CloudFormation stack's
//! HTTP api destroyed while a client kept polling the api's url):
//!
//! > An address of the `execute-api` endpoint space whose api id names no api is answered by
//! > the space ITSELF — never by the front door's "no service claimed this" refusal, since
//! > API Gateway published that address — and which of its two answers it gets depends on
//! > whether the address names a STAGE to place: a path carrying a segment names one, and the
//! > space turns that request away with `403` `{"message":"Forbidden"}` without saying whether
//! > the api or the stage exists, while the endpoint's ROOT names no stage anywhere and is
//! > answered `404` `{"message":"Not Found"}` — the same answer a LIVE api's root gives when
//! > no stage of it serves there. An HTTP api's url IS that root (`Api$ApiEndpoint`, "the
//! > stage name is typically appended to this URI"), so a client that deletes an api and keeps
//! > fetching the url it was given reads the `404` from then on.
//!
//! The rows differ along what the rule generalises over: WHY the id resolves to nothing (an
//! api that was created, served and then deleted, and an id nothing ever minted), the
//! addressing that carried it (the `execute-api` host AWS documents, the emulator's own
//! rewriting of it with the api id behind the reserved marker, and the rewriting that carries
//! the endpoint HOST behind that marker), how the root is written (a bare `/` and the empty
//! path), the method (a plain `GET` and the `OPTIONS` preflight a browser sends, which no
//! `CorsConfiguration` of a deleted api is left to answer), and — as the contrast — the paths
//! that DO name a stage and the live api's own root, which keeps serving.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, REGION};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The emulator's own endpoint: the address a client on this machine really dials, which
/// names no api, so such a client carries the endpoint in the path instead.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// The answer the endpoint space owes the ROOT of an endpoint that resolves to no api: the
/// endpoint holds nothing, and nothing in the address names a stage to place.
const NOT_FOUND: (u16, &str) = (404, "Not Found");

/// The answer it owes an address that DOES name a stage: placing `(api, stage)` is what
/// failed, and the space says nothing about whether either exists.
const FORBIDDEN: (u16, &str) = (403, "Forbidden");

/// The status the live api's route answers: its `HTTP_PROXY` integration names "an endpoint
/// over the network" (`Integration$IntegrationUri`) this emulator opens no connection to, so
/// the caller is sent there with the redirect that preserves its method and body.
const TEMPORARY_REDIRECT: u16 = 307;

/// The upstream that integration names, so a served answer is told from every refusal by the
/// address it carries.
const UPSTREAM: &str = "https://upstream.example.com/orders";

/// The error code the front door answers when NO service claims a request: the one answer an
/// `execute-api` address must never get, since it is the emulator refusing an address API
/// Gateway itself published.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The origin a preflight row carries, and the method it asks about: a browser's preflight of
/// the api's root.
const ORIGIN: &str = "https://app.example.com";
const REQUESTED_METHOD: &str = "GET";

/// How a client ADDRESSES an api's endpoint. The three spellings this data plane reads, so
/// the rule is asserted at every address a client can hold rather than at one of them.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// The host `Api$ApiEndpoint` documents: `{api-id}.execute-api.{region}.amazonaws.com`.
    OwnHost,
    /// The emulator's own endpoint, with the api id behind the reserved marker.
    MarkedApiId,
    /// The emulator's own endpoint, with the endpoint HOST behind the reserved marker.
    MarkedHost,
}

impl Addressing {
    /// The `Host` a client sends and the path it carries, for the api `api_id` and the invoke
    /// path `path`.
    fn request(self, api_id: &str, path: &str) -> (String, String) {
        let endpoint_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
        match self {
            Self::OwnHost => (endpoint_host, path.to_string()),
            Self::MarkedApiId => (
                EMULATOR_ENDPOINT.to_string(),
                format!("/_aws/execute-api/{api_id}{path}"),
            ),
            Self::MarkedHost => (
                EMULATOR_ENDPOINT.to_string(),
                format!("/_aws/{endpoint_host}{path}"),
            ),
        }
    }

    /// Every addressing, for a table that asserts a row at each of them.
    const ALL: [Self; 3] = [Self::OwnHost, Self::MarkedApiId, Self::MarkedHost];
}

/// One api, created with `route_key` over an `HTTP_PROXY` integration and the auto-deploying
/// stage `stage` — so a request the route matches is SERVED, which is what makes an address's
/// answer after a delete a change and not an absence.
async fn serve_api(
    emulator: &MultiServiceRouter,
    case: &str,
    stage: &str,
    route_key: &str,
) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": format!("endpoint-root-{case}"), "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {created}"
    );
    let api_id = as_str(&created, "apiId");

    let (status, integration) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": UPSTREAM,
            "integrationMethod": "ANY",
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {integration}"
    );
    let integration_id = as_str(&integration, "integrationId");

    let (status, route) = call(
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
        "case {case:?}: CreateRoute must answer 201, got {route}"
    );

    let (status, stage) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": stage, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {stage}"
    );
    api_id
}

/// One request to `api_id` at `addressing`, with the invoke path `path` and the headers the
/// row carries.
async fn fetch(
    emulator: &MultiServiceRouter,
    addressing: Addressing,
    api_id: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let (host, uri) = addressing.request(api_id, path);
    send(emulator, &host, method, &uri, headers).await
}

/// Assert `answer` is the endpoint space's own `(status, message)` and not the front door's
/// refusal — which is what tells the space answering an address it published from the
/// emulator declining to place one.
fn answered(case: &str, answer: &Answer, expected: (u16, &str)) {
    assert_ne!(
        answer.error_type.as_deref(),
        Some(UNKNOWN_SERVICE),
        "case {case:?}: the endpoint space must answer an address it published, got {}",
        answer.summary()
    );
    assert_eq!(
        (answer.status, message(answer).as_str()),
        expected,
        "case {case:?}: expected the endpoint space's {expected:?}, got {}",
        answer.summary()
    );
}

/// One row of the table: how the invoke path is written, and what the endpoint space owes it
/// once the api id resolves to nothing.
struct PathRow {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    headers: &'static [(&'static str, &'static str)],
    expected: (u16, &'static str),
}

/// The paths an api's endpoint is addressed at, and what each is owed once the id behind it
/// names no api: the ROOT in both spellings and under both methods a client of an HTTP api's
/// url sends, and the paths that name a stage.
const PATH_ROWS: &[PathRow] = &[
    PathRow {
        case: "the endpoint root",
        method: "GET",
        path: "/",
        headers: &[],
        expected: NOT_FOUND,
    },
    PathRow {
        case: "the endpoint root written as the empty path",
        method: "GET",
        path: "",
        headers: &[],
        expected: NOT_FOUND,
    },
    PathRow {
        case: "a preflight of the endpoint root",
        method: "OPTIONS",
        path: "/",
        headers: &[
            ("origin", ORIGIN),
            ("access-control-request-method", REQUESTED_METHOD),
        ],
        expected: NOT_FOUND,
    },
    PathRow {
        case: "a path naming one segment, which is a stage name",
        method: "GET",
        path: "/orders",
        headers: &[],
        expected: FORBIDDEN,
    },
    PathRow {
        case: "a path naming a stage and a route beneath it",
        method: "GET",
        path: "/prod/orders",
        headers: &[],
        expected: FORBIDDEN,
    },
];

#[tokio::test]
async fn the_root_of_an_endpoint_that_resolves_to_no_api_is_not_found() {
    let emulator = emulator();

    // The id of an api that existed: it is created, its root is SERVED, and then it is
    // deleted — so every answer below is the delete's own consequence at an address that
    // worked a moment earlier.
    let deleted = serve_api(&emulator, "deleted", "$default", "$default").await;
    for addressing in Addressing::ALL {
        let case = format!("{addressing:?}: the live api's own root");
        let served = fetch(&emulator, addressing, &deleted, "GET", "/", &[]).await;
        assert_eq!(
            served.status,
            TEMPORARY_REDIRECT,
            "case {case:?}: the api's root must be served by its $default route, got {}",
            served.summary()
        );
        assert_eq!(
            served.location.as_deref(),
            Some(UPSTREAM),
            "case {case:?}: the served answer must name the integration's upstream, got {}",
            served.summary()
        );
    }
    let (status, body) = call(&emulator, "DELETE", &format!("/v2/apis/{deleted}"), None).await;
    assert_eq!(
        status, 204,
        "DeleteApi must answer 204 with no body, got {status} {body}"
    );

    // And an id nothing ever minted, which is the same fact to an endpoint space that reads
    // nothing of an api for either. Shaped like an api id (`Id` is an unconstrained string in
    // this model) and never minted by a case here.
    let never_minted = "zz9999zz99";

    for api_id in [deleted.as_str(), never_minted] {
        let why = if api_id == never_minted {
            "an id nothing minted"
        } else {
            "a deleted api"
        };
        for addressing in Addressing::ALL {
            for row in PATH_ROWS {
                let case = format!("{why}, {addressing:?}: {}", row.case);
                let answer = fetch(
                    &emulator,
                    addressing,
                    api_id,
                    row.method,
                    row.path,
                    row.headers,
                )
                .await;
                answered(&case, &answer, row.expected);
                // Nothing of an api was read for the address, so nothing states a CORS rule
                // for it either: a preflight of a gone api carries no allow-origin header.
                assert_eq!(
                    answer.header("access-control-allow-origin"),
                    None,
                    "case {case:?}: an address that resolves no api states no CORS header, \
                     got {}",
                    answer.summary()
                );
            }
        }
    }

    // The contrast that makes the `404` an answer about the ENDPOINT rather than a blanket
    // one: a LIVE api whose deployment serves its root keeps serving it, and the root of a
    // live api that holds NO stage serving it is its own `404` — the very answer the gone
    // api's root now gives, as it must be.
    let live = serve_api(&emulator, "live", "$default", "$default").await;
    let named_stage_only = serve_api(&emulator, "named", "prod", "GET /orders").await;
    for addressing in Addressing::ALL {
        let case = format!("{addressing:?}: a live api's root");
        let served = fetch(&emulator, addressing, &live, "GET", "/", &[]).await;
        assert_eq!(
            served.status,
            TEMPORARY_REDIRECT,
            "case {case:?}: a live api's root must stay served, got {}",
            served.summary()
        );
        let case = format!("{addressing:?}: a live api's own route under its named stage");
        let served = fetch(
            &emulator,
            addressing,
            &named_stage_only,
            "GET",
            "/prod/orders",
            &[],
        )
        .await;
        assert_eq!(
            served.status,
            TEMPORARY_REDIRECT,
            "case {case:?}: the named stage must serve its route, got {}",
            served.summary()
        );
        let case = format!("{addressing:?}: the root of a live api no stage serves");
        let unserved = fetch(&emulator, addressing, &named_stage_only, "GET", "/", &[]).await;
        answered(&case, &unserved, NOT_FOUND);
    }
}
