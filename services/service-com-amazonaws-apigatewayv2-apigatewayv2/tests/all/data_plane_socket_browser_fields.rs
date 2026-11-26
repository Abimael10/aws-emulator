// SPDX-License-Identifier: CC0-1.0

//! What a request's BROWSER-SET fields decide at one of this service's core-owned data-plane
//! sockets — over a real connection to the address the control plane published.
//!
//! RULE: **an `Origin` or `Sec-Fetch-Site` field a request carries never decides whether one
//! of this service's data-plane addresses serves it. The api's records decide the answer — the
//! stage, the deployment, the route, its authorization and its integration — and the api's own
//! `CorsConfiguration` decides only what DECORATES that answer: the origin is echoed in
//! `Access-Control-Allow-Origin` (beside the headers `ExposeHeaders` names) when
//! `AllowOrigins` admits it, and no `Access-Control-*` field is written when it does not.**
//!
//! That is what the member exists for: `Cors$AllowOrigins` is "represents a collection of
//! allowed origins" of a configuration whose own documentation calls it "a CORS configuration",
//! and CORS is a rule about what a BROWSER may do with an answer, not about which requests the
//! gateway accepts — so a request from an origin the configuration does not list is served and
//! merely answered without the grant, and the backend sees it either way.
//!
//! An address of this service that answered such a request itself would make the api's
//! configuration unobservable: every leg of it — the grant, the absence of the grant, the
//! preflight, and the body the route's function returns — is only reachable once the request
//! gets past the address to the api. This suite drives those legs over a REAL socket, because
//! the address the control plane publishes is one a client dials, and nothing of what it
//! answers is read off an emulator internal: every served row is answered by a FUNCTION that
//! ran, and reports back the `Origin` it was given.
//!
//! The rows span what the rule generalises over: a request carrying no browser-set field at all
//! (an SDK or CLI), one carrying an origin the configuration admits, one carrying an origin it
//! does not, the `null` origin a sandboxed document sends, the browser's own
//! `Sec-Fetch-Site` classifications beside each of those (`same-origin`, `same-site`,
//! `cross-site`, `none`, and a value no browser spells), a `Sec-Fetch-Site` carried with no
//! `Origin` at all, and the `OPTIONS` preflight a browser sends ahead of the actual request.
//!
//! What the decoration itself is — every `Cors` member, the wildcard, an api carrying no
//! configuration — is `http_api_cors_data_plane`'s subject, and the addressings of a custom
//! domain name are `custom_domain_mapping_invoke`'s. What is added here is the leg neither
//! carries: the fields a BROWSER sets reaching one of this service's own sockets.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_domain_mapping_invoke::{control, given_domain_name, given_mapping};
use crate::http_api_backend_dispatch::{emulator_with_lambda, given_function, lambda_integration};
// The ONE raw HTTP/1.1 client the suites that drive this service's core-owned addresses use,
// so none of them reads an answer differently from another.
use crate::published_invoke_socket_front_door::{request, Answered};

/// The status the route's function answers with, and the marker its body carries: an answer
/// holding them is one the FUNCTION produced rather than one an address decided.
const SERVED: u16 = 200;
const MARKER: &str = "served-by-the-function";

/// The status the gateway answers a CORS preflight no route of the api matches: the api's one
/// route catches the actual `GET` only, so the configuration's own headers are the whole
/// answer.
const PREFLIGHT: u16 = 204;

/// The origin the api's `CorsConfiguration` admits, and one it does not.
const ALLOWED_ORIGIN: &str = "https://app.example.com";
const FOREIGN_ORIGIN: &str = "https://other.example.test";

/// The origin a sandboxed document, a `file://` page and some redirects send. It is an origin
/// like any other to a configuration that does not list it.
const NULL_ORIGIN: &str = "null";

/// The header the api's `ExposeHeaders` names, as an actual request's answer joins them, and
/// the ones its `AllowHeaders` names, as a preflight's answer joins them.
const EXPOSED_HEADERS: &str = "x-app-marker";
const ALLOWED_HEADERS: &str = "authorization,content-type";

/// The one method the configuration allows, which is also the route's.
const ALLOWED_METHOD: &str = "GET";

/// The route the api carries and the path within its stage that matches it.
const ROUTE_KEY: &str = "GET /hit";
const ROUTE_PATH: &str = "/hit";

/// The prefix of every response header the decoration is made of, so an answer can be asserted
/// to carry EXACTLY what the configuration states and nothing else.
const CORS_HEADER_PREFIX: &str = "access-control-";

/// The custom domain name the api is reached under, whose own socket is the address every row
/// below dials.
const DOMAIN: &str = "shop.api.example.com";

/// The handler the api fronts: it answers the marker above and reports the `Origin` it was
/// given, so a row shows both that the function ran and what reached it.
const REPORTS_THE_ORIGIN: &str = "import json\n\
     def lambda_handler(event, context):\n\
     \x20   headers = event.get('headers') or {}\n\
     \x20   return {\n\
     \x20       'statusCode': 200,\n\
     \x20       'headers': {'content-type': 'application/json'},\n\
     \x20       'body': json.dumps({\n\
     \x20           'marker': 'served-by-the-function',\n\
     \x20           'origin': headers.get('origin', ''),\n\
     \x20       }),\n\
     \x20   }\n";

/// Every `Access-Control-*` field an answer carries, sorted, so a row asserts the WHOLE
/// decoration rather than a subset of it.
///
/// `pub(crate)` with [`expected`] below, because every suite that asserts what an api's
/// `CorsConfiguration` decorated an answer with reads that decoration off the answer the same
/// way — `published_endpoint_cors_browser_fields` drives the same rule at the address an api
/// PUBLISHES.
pub(crate) fn decoration(answered: &Answered) -> Vec<(String, String)> {
    let mut carried: Vec<(String, String)> = answered
        .headers
        .iter()
        .filter(|(name, _)| name.starts_with(CORS_HEADER_PREFIX))
        .cloned()
        .collect();
    carried.sort();
    carried
}

/// `pairs` as a sorted decoration, for a row to state what the answer must carry.
pub(crate) fn expected(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    owned.sort();
    owned
}

/// The decoration an ACTUAL request from the allow-listed origin earns: that origin echoed
/// back, and the headers the configuration exposes to the page.
fn granted() -> Vec<(String, String)> {
    expected(&[
        ("access-control-allow-origin", ALLOWED_ORIGIN),
        ("access-control-expose-headers", EXPOSED_HEADERS),
    ])
}

/// The decoration a PREFLIGHT from that origin earns: the origin, and the methods and headers
/// the configuration allows.
fn preflight_grant() -> Vec<(String, String)> {
    expected(&[
        ("access-control-allow-origin", ALLOWED_ORIGIN),
        ("access-control-allow-methods", ALLOWED_METHOD),
        ("access-control-allow-headers", ALLOWED_HEADERS),
    ])
}

/// Send one request to the address the custom domain name published, under the DOMAIN NAME as
/// the `Host` — which is what a client that resolved the domain sends — through the shared
/// client every suite that drives these addresses uses.
async fn request_with(
    case: &str,
    authority: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Answered {
    request(case, authority, method, path, DOMAIN, headers).await
}

/// Create the api the rows are served by: the `CorsConfiguration` above, an `AWS_PROXY`
/// integration over `function_arn`, the one route that fronts it, and an auto-deploying stage
/// that serves the api's root. Answers its `ApiId`.
async fn given_cors_api(emulator: &MultiServiceRouter, case: &str, function_arn: &str) -> String {
    let (status, api) = control(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": "browser-fields-reach-the-api",
            "protocolType": "HTTP",
            "corsConfiguration": {
                "allowOrigins": [ALLOWED_ORIGIN],
                "allowMethods": [ALLOWED_METHOD],
                "allowHeaders": ["authorization", "content-type"],
                "exposeHeaders": [EXPOSED_HEADERS]
            }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = api
        .get("apiId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateApi answered no apiId: {api}"))
        .to_string();

    let (status, integration) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(lambda_integration(function_arn, "2.0")),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration answered {status} {integration}"
    );
    let integration_id = integration
        .get("integrationId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateIntegration answered no integrationId: {integration}")
        })
        .to_string();

    let (status, route) = control(
        emulator,
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
        "case {case:?}: CreateRoute answered {status} {route}"
    );

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
    api_id
}

/// One row: the browser-set fields the request carries, and what the api's records answer it.
struct Row {
    case: &'static str,
    method: &'static str,
    /// The fields the client sends beside the `Host`.
    headers: &'static [(&'static str, &'static str)],
    status: u16,
    /// The origin the FUNCTION must report having been given, or `None` for a row no route
    /// matches, which the gateway answers itself.
    seen_by_the_function: Option<&'static str>,
    /// Every `Access-Control-*` field the answer must carry.
    decoration: fn() -> Vec<(String, String)>,
}

/// The decoration a request the configuration grants nothing earns: none at all.
fn ungranted() -> Vec<(String, String)> {
    Vec::new()
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client that sets no browser field at all",
            method: ALLOWED_METHOD,
            headers: &[],
            status: SERVED,
            seen_by_the_function: Some(""),
            decoration: ungranted,
        },
        Row {
            case: "an origin the configuration admits, with no Sec-Fetch-Site",
            method: ALLOWED_METHOD,
            headers: &[("Origin", ALLOWED_ORIGIN)],
            status: SERVED,
            seen_by_the_function: Some(ALLOWED_ORIGIN),
            decoration: granted,
        },
        Row {
            case: "that origin with the browser's own cross-site classification",
            method: ALLOWED_METHOD,
            headers: &[("Origin", ALLOWED_ORIGIN), ("Sec-Fetch-Site", "cross-site")],
            status: SERVED,
            seen_by_the_function: Some(ALLOWED_ORIGIN),
            decoration: granted,
        },
        Row {
            case: "that origin classified same-site",
            method: ALLOWED_METHOD,
            headers: &[("Origin", ALLOWED_ORIGIN), ("Sec-Fetch-Site", "same-site")],
            status: SERVED,
            seen_by_the_function: Some(ALLOWED_ORIGIN),
            decoration: granted,
        },
        Row {
            case: "that origin classified same-origin",
            method: ALLOWED_METHOD,
            headers: &[("Origin", ALLOWED_ORIGIN), ("Sec-Fetch-Site", "same-origin")],
            status: SERVED,
            seen_by_the_function: Some(ALLOWED_ORIGIN),
            decoration: granted,
        },
        Row {
            case: "that origin classified none, as a document the user opened sends it",
            method: ALLOWED_METHOD,
            headers: &[("Origin", ALLOWED_ORIGIN), ("Sec-Fetch-Site", "none")],
            status: SERVED,
            seen_by_the_function: Some(ALLOWED_ORIGIN),
            decoration: granted,
        },
        Row {
            case: "that origin with a Sec-Fetch-Site no browser spells",
            method: ALLOWED_METHOD,
            headers: &[
                ("Origin", ALLOWED_ORIGIN),
                ("Sec-Fetch-Site", "not-a-classification"),
            ],
            status: SERVED,
            seen_by_the_function: Some(ALLOWED_ORIGIN),
            decoration: granted,
        },
        Row {
            case: "an origin the configuration does not admit",
            method: ALLOWED_METHOD,
            headers: &[("Origin", FOREIGN_ORIGIN), ("Sec-Fetch-Site", "cross-site")],
            status: SERVED,
            seen_by_the_function: Some(FOREIGN_ORIGIN),
            decoration: ungranted,
        },
        Row {
            case: "the null origin a sandboxed document sends",
            method: ALLOWED_METHOD,
            headers: &[("Origin", NULL_ORIGIN), ("Sec-Fetch-Site", "cross-site")],
            status: SERVED,
            seen_by_the_function: Some(NULL_ORIGIN),
            decoration: ungranted,
        },
        Row {
            case: "a cross-site classification carried with no origin at all",
            method: ALLOWED_METHOD,
            headers: &[("Sec-Fetch-Site", "cross-site")],
            status: SERVED,
            seen_by_the_function: Some(""),
            decoration: ungranted,
        },
        Row {
            case: "the preflight a browser sends ahead of the actual request",
            method: "OPTIONS",
            headers: &[
                ("Origin", ALLOWED_ORIGIN),
                ("Sec-Fetch-Site", "cross-site"),
                ("Access-Control-Request-Method", ALLOWED_METHOD),
                ("Access-Control-Request-Headers", ALLOWED_HEADERS),
            ],
            status: PREFLIGHT,
            seen_by_the_function: None,
            decoration: preflight_grant,
        },
    ]
}

/// RULE: the browser-set fields of a request reaching one of this service's data-plane sockets
/// decide nothing about whether it is served — the api's records answer it, and the api's
/// `CorsConfiguration` decides only the `Access-Control-*` decoration of that answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_browsers_own_fields_decide_only_the_decoration_of_what_the_api_answers() {
    let setup = "an api with a CorsConfiguration behind a custom domain name";
    let (emulator, engine) = emulator_with_lambda();
    let function = given_function(
        &emulator,
        &engine,
        setup,
        "browser-fields-echo",
        REPORTS_THE_ORIGIN,
    )
    .await;
    let api_id = given_cors_api(&emulator, setup, &function).await;
    let authority = given_domain_name(&emulator, setup, DOMAIN).await;
    given_mapping(&emulator, setup, DOMAIN, &api_id, "$default", None).await;

    for row in rows() {
        let case = row.case;
        let answered = request_with(case, &authority, row.method, ROUTE_PATH, row.headers).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected the address to answer {}, got {answered:?}",
            row.status
        );
        match row.seen_by_the_function {
            Some(origin) => {
                let expected = json!({ "marker": MARKER, "origin": origin });
                assert_eq!(
                    answered.json(),
                    expected,
                    "case {case:?}: expected the function's own answer {expected}, got \
                     {answered:?}"
                );
            }
            None => assert!(
                answered.body.is_empty(),
                "case {case:?}: expected the gateway's own preflight answer to carry no body, \
                 got {answered:?}"
            ),
        }
        let carried = decoration(&answered);
        let wanted = (row.decoration)();
        assert_eq!(
            carried, wanted,
            "case {case:?}: expected the CORS decoration {wanted:?}, got {carried:?}"
        );
    }
}
