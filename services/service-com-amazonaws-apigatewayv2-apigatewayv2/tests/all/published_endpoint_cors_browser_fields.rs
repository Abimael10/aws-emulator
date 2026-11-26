// SPDX-License-Identifier: CC0-1.0

//! What an `Origin` decides at the address an HTTP api PUBLISHES, over a real connection to it.
//!
//! RULE: **an `Origin` (or `Sec-Fetch-Site`) a request carries never decides whether an HTTP
//! api's data plane serves it: the request is served on its own merits — the stage, the route,
//! that route's `AuthorizationType` and its integration — and the ONLY Origin-dependent
//! difference is the `Access-Control-*` decoration the api's own `CorsConfiguration` owns,
//! which REPLACES whatever the integration returned for those fields and is absent entirely
//! for an origin `AllowOrigins` does not admit. Where this surface does refuse, the refusal is
//! the gateway's own JSON envelope — `403 {"message":"Forbidden"}` for an `AWS_IAM` route
//! reached with no credential, `404 {"message":"Not Found"}` for an address the api serves
//! nothing at — decorated by the same configuration.**
//!
//! That is what the member exists for: `Api$CorsConfiguration` is "a CORS configuration.
//! Supported only for HTTP APIs" whose `Cors$AllowOrigins` is "represents a collection of
//! allowed origins", and CORS is a rule about what a BROWSER may do with an ANSWER rather than
//! about which requests a gateway accepts — so a request from an origin the collection does
//! not hold is served like any other and merely answered without the grant, and the route's
//! backend sees it either way.
//!
//! Why this suite exists beside the in-process ones and beside
//! `data_plane_socket_browser_fields` (which drives a CUSTOM DOMAIN NAME's socket): the
//! `ApiEndpoint` an HTTP api publishes is a SOCKET of this service's own
//! (`crate::service::invoke_plane_socket`) whenever this emulator is served on a port at all,
//! and a request arriving there is read and handed to the router by this service's own
//! endpoint rather than by this emulator's front door. An address that answered an
//! Origin-bearing request itself would make every leg of the api's configuration unreachable,
//! so each leg is driven HERE, at the published address, with the route's function actually
//! running.
//!
//! The rows span what the rule generalises over: which browser-set fields the request carries
//! (none at all, an origin the configuration admits, one it does not, the `null` origin a
//! sandboxed document sends, a `Sec-Fetch-Site` classification beside each of those and one
//! carried with no origin), whether the request is a preflight and whether the method it asks
//! about is admitted, whether a route of the api MATCHES that preflight (so it is routed and
//! authorized like any other request) or none does (so the gateway answers it), what the
//! route's `AuthorizationType` requires of the caller, which `Access-Control-*` fields the
//! INTEGRATION returned for itself, and both sides of `DeleteCorsConfiguration`.
//!
//! The decoration's own table — every `Cors` member, the wildcard, an api carrying no
//! configuration — is `http_api_cors_data_plane`'s subject.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{control, given_function, lambda_integration};
// The ONE reading of the `Access-Control-*` decoration off an answer the suites that assert it
// share, beside the ONE raw HTTP/1.1 client the suites that drive this service's core-owned
// addresses use and the ONE statement of what an api PUBLISHES and of the emulator that
// publishes it.
use crate::data_plane_socket_browser_fields::{decoration, expected};
use crate::published_invoke_socket_front_door::{
    announced_parts, member, request, served_emulator, Answered,
};

/// The status the route's function answers with, and the marker its body and its own header
/// field carry: an answer holding them is one the FUNCTION produced rather than one the
/// address decided.
const SERVED: u16 = 200;
const MARKER: &str = "backend-served";

/// The status the GATEWAY answers a CORS preflight NO ROUTE of the api matches.
const PREFLIGHT: u16 = 204;

/// The status and sentence an `AWS_IAM` route reached with no AWS credential earns, and the
/// ones an address the api serves nothing at earns.
const FORBIDDEN: u16 = 403;
const FORBIDDEN_MESSAGE: &str = "Forbidden";
const NOT_FOUND: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Not Found";

/// The origin the api's `CorsConfiguration` admits, and one it does not.
const ALLOWED_ORIGIN: &str = "https://app.example.com";
const FOREIGN_ORIGIN: &str = "https://other.example.test";

/// The origin a sandboxed document, a `file://` page and some redirects send. It is an origin
/// like any other to a configuration that does not list it.
const NULL_ORIGIN: &str = "null";

/// The one method the configuration's `AllowMethods` admits — which is also the route's — and
/// a method it does not, for the preflight that asks about one.
const ALLOWED_METHOD: &str = "GET";
const UNADMITTED_METHOD: &str = "POST";

/// The `AllowHeaders` the configuration names, as a preflight's answer joins them, and the
/// `ExposeHeaders` it names, as an actual request's answer joins them. The exposed field is
/// the one the FUNCTION also writes, which is what makes the gateway's ownership of the
/// `Access-Control-*` fields — and only of those — observable.
const ALLOWED_HEADERS: &str = "content-type";
const EXPOSED_HEADERS: &str = "x-marker";

/// The `MaxAge` the configuration carries, inside the modelled `range` of
/// `Cors$MaxAge` (-1..86400), as a preflight's answer states it.
const MAX_AGE: i64 = 600;

/// The field the function writes beside the marker in its body: a response field of the
/// backend's that is NOT an `Access-Control-*` one, so it must reach the client untouched.
const BACKEND_FIELD: &str = "x-marker";

/// The `Access-Control-*` fields the FUNCTION returns for itself. Neither value may ever be
/// seen by a client of an api that carries a configuration: the gateway owns those fields.
const BACKEND_ALLOW_ORIGIN: &str = "https://backend.example.invalid";
const BACKEND_EXPOSE_HEADERS: &str = "x-the-backend-chose-this";

/// The route the decorated api carries and the path within its stage that matches it.
const ROUTE_KEY: &str = "GET /r";
const ROUTE_PATH: &str = "/r";

/// The catch-all route key an api declares to catch every method and path, which is what makes
/// it match a browser's `OPTIONS` preflight as well.
const CATCH_ALL_ROUTE_KEY: &str = "ANY /{proxy+}";

/// The reserved stage "that serves an api's root", reached with no stage segment at all.
const DEFAULT_STAGE: &str = "$default";

/// The handler every api below fronts: it answers the marker, reports the `Origin` it was
/// given — so a row shows both that the function ran and what reached it — and writes
/// `Access-Control-*` fields of its OWN beside a field that is not one.
const REPORTS_THE_ORIGIN: &str = "import json\n\
     def lambda_handler(event, context):\n\
     \x20   headers = event.get('headers') or {}\n\
     \x20   return {\n\
     \x20       'statusCode': 200,\n\
     \x20       'headers': {\n\
     \x20           'content-type': 'application/json',\n\
     \x20           'x-marker': 'backend-served',\n\
     \x20           'access-control-allow-origin': 'https://backend.example.invalid',\n\
     \x20           'access-control-expose-headers': 'x-the-backend-chose-this',\n\
     \x20       },\n\
     \x20       'body': json.dumps({\n\
     \x20           'marker': 'backend-served',\n\
     \x20           'origin': headers.get('origin', ''),\n\
     \x20       }),\n\
     \x20   }\n";

/// The same handler WITHOUT any `Access-Control-*` field of its own, for the legs that assert
/// an api which carries no configuration decorates nothing at all.
const REPORTS_THE_ORIGIN_UNDECORATED: &str = "import json\n\
     def lambda_handler(event, context):\n\
     \x20   headers = event.get('headers') or {}\n\
     \x20   return {\n\
     \x20       'statusCode': 200,\n\
     \x20       'headers': {\n\
     \x20           'content-type': 'application/json',\n\
     \x20           'x-marker': 'backend-served',\n\
     \x20       },\n\
     \x20       'body': json.dumps({\n\
     \x20           'marker': 'backend-served',\n\
     \x20           'origin': headers.get('origin', ''),\n\
     \x20       }),\n\
     \x20   }\n";

/// The values the rows assert on are the ones the handlers above actually write, so a change to
/// one of the constants cannot leave the handler and the assertions stating different things.
#[test]
fn the_handlers_write_the_values_the_rows_assert_on() {
    let both: &[(&str, &str)] = &[
        ("the decorating handler", REPORTS_THE_ORIGIN),
        ("the undecorated handler", REPORTS_THE_ORIGIN_UNDECORATED),
    ];
    for (case, source) in both {
        for value in [MARKER, BACKEND_FIELD] {
            assert!(
                source.contains(value),
                "case {case:?}: the handler must write {value:?}, got {source:?}"
            );
        }
    }
    for value in [BACKEND_ALLOW_ORIGIN, BACKEND_EXPOSE_HEADERS] {
        assert!(
            REPORTS_THE_ORIGIN.contains(value),
            "the decorating handler must write {value:?}, got {REPORTS_THE_ORIGIN:?}"
        );
        assert!(
            !REPORTS_THE_ORIGIN_UNDECORATED.contains(value),
            "the undecorated handler must write no {value:?}, got \
             {REPORTS_THE_ORIGIN_UNDECORATED:?}"
        );
    }
}

/// Every `Access-Control-*` field an answer carries is read with the one reading every suite
/// that asserts a decoration shares ([`decoration`]); `pairs` is stated as a sorted decoration
/// through the same suite's [`expected`].
///
/// The decoration an ACTUAL request from the admitted origin earns: that origin echoed back,
/// and the fields the configuration exposes to the page.
fn granted() -> Vec<(String, String)> {
    expected(&[
        ("access-control-allow-origin", ALLOWED_ORIGIN),
        ("access-control-expose-headers", EXPOSED_HEADERS),
    ])
}

/// The decoration a PREFLIGHT from that origin, asking about a method the configuration
/// admits, earns: the origin, the methods and headers it allows, and the `MaxAge` it states.
fn preflight_grant() -> Vec<(String, String)> {
    expected(&[
        ("access-control-allow-origin", ALLOWED_ORIGIN),
        ("access-control-allow-methods", ALLOWED_METHOD),
        ("access-control-allow-headers", ALLOWED_HEADERS),
        ("access-control-max-age", &MAX_AGE.to_string()),
    ])
}

/// The decoration a request the configuration grants nothing earns: none at all — including
/// none of the fields the function returned for itself.
fn ungranted() -> Vec<(String, String)> {
    Vec::new()
}

/// The `CorsConfiguration` every decorated api below carries.
fn cors_configuration() -> Value {
    json!({
        "allowOrigins": [ALLOWED_ORIGIN],
        "allowMethods": [ALLOWED_METHOD],
        "allowHeaders": [ALLOWED_HEADERS],
        "exposeHeaders": [EXPOSED_HEADERS],
        "maxAge": MAX_AGE
    })
}

/// One api of a case, as a client of its published `ApiEndpoint` reaches it.
struct Published {
    api_id: String,
    /// The `host:port` the published endpoint names, which is what a client dials.
    authority: String,
    /// The path the api is published under at that address; a route path is appended to it.
    base: String,
}

impl Published {
    /// The invoke URL path for `route_path` at the published endpoint, which addresses the
    /// stage that serves the api's root.
    fn at(&self, route_path: &str) -> String {
        format!("{}{route_path}", self.base)
    }
}

/// Create an api carrying `cors` (or none), one `AWS_PROXY` integration over `function_arn`,
/// one route `route_key` with `authorization_type`, and an auto-deploying stage that serves the
/// api's root. Answers the address the api published.
async fn given_api(
    emulator: &MultiServiceRouter,
    case: &str,
    name: &str,
    function_arn: &str,
    cors: Option<Value>,
    route_key: &str,
    authorization_type: &str,
) -> Published {
    let mut body = json!({ "name": name, "protocolType": "HTTP" });
    if let Some(cors) = cors {
        body["corsConfiguration"] = cors;
    }
    let (status, api) = control(emulator, "POST", "/v2/apis", Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi answered {status} {api}"
    );
    let api_id = member(case, &api, "apiId");
    let announced = member(case, &api, "apiEndpoint");

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
    let integration_id = member(case, &integration, "integrationId");

    let (status, route) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": route_key,
            "authorizationType": authorization_type,
            "target": format!("integrations/{integration_id}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute {route_key:?} answered {status} {route}"
    );

    let (status, stage) = control(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": DEFAULT_STAGE, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage answered {status} {stage}"
    );

    let (authority, base) = announced_parts(case, &api_id, &announced);
    Published {
        api_id,
        authority,
        base,
    }
}

/// Send one request to the published address, with that address as the `Host` — which is what
/// a client that read `ApiEndpoint` sends.
async fn sent(
    case: &str,
    api: &Published,
    method: &str,
    route_path: &str,
    headers: &[(&str, &str)],
) -> Answered {
    request(
        case,
        &api.authority,
        method,
        &api.at(route_path),
        &api.authority,
        headers,
    )
    .await
}

/// What the function answered, as the body it returns for the origin it was given.
fn functions_answer(origin: &str) -> Value {
    json!({ "marker": MARKER, "origin": origin })
}

/// One row: the browser-set fields the request carries, and what the api's records answer it.
struct Row {
    case: &'static str,
    method: &'static str,
    /// The fields the client sends beside the `Host`.
    headers: &'static [(&'static str, &'static str)],
    status: u16,
    /// The origin the FUNCTION must report having been given, or `None` for a request no route
    /// matches, which the gateway answers itself without reaching the integration.
    seen_by_the_function: Option<&'static str>,
    /// Every `Access-Control-*` field the answer must carry.
    decoration: fn() -> Vec<(String, String)>,
}

/// The rows of the served api: one route, `GET /r`, which matches no `OPTIONS` request — so
/// every preflight among them is the GATEWAY's own answer.
fn served_rows() -> Vec<Row> {
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
            case: "that origin classified same-origin",
            method: ALLOWED_METHOD,
            headers: &[("Origin", ALLOWED_ORIGIN), ("Sec-Fetch-Site", "same-origin")],
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
        Row {
            case: "that preflight from an origin the configuration does not admit",
            method: "OPTIONS",
            headers: &[
                ("Origin", FOREIGN_ORIGIN),
                ("Sec-Fetch-Site", "cross-site"),
                ("Access-Control-Request-Method", ALLOWED_METHOD),
            ],
            status: PREFLIGHT,
            seen_by_the_function: None,
            decoration: ungranted,
        },
        Row {
            case: "a preflight asking about a method the configuration does not admit",
            method: "OPTIONS",
            headers: &[
                ("Origin", ALLOWED_ORIGIN),
                ("Sec-Fetch-Site", "cross-site"),
                ("Access-Control-Request-Method", UNADMITTED_METHOD),
            ],
            status: PREFLIGHT,
            seen_by_the_function: None,
            decoration: ungranted,
        },
    ]
}

/// RULE: at the address an HTTP api published, an `Origin` decides nothing about whether the
/// request is served — the route's function answers it either way, and the api's
/// `CorsConfiguration` decides only the `Access-Control-*` decoration of that answer, which
/// replaces the fields the function returned for itself.
#[tokio::test(flavor = "multi_thread")]
async fn an_origin_decides_only_the_decoration_of_what_the_published_address_answers() {
    let setup = "an api with a CorsConfiguration over a function that decorates its own answer";
    let (emulator, engine) = served_emulator();
    let function = given_function(
        &emulator,
        &engine,
        setup,
        "published-cors-origin-echo",
        REPORTS_THE_ORIGIN,
    )
    .await;
    let api = given_api(
        &emulator,
        setup,
        "published-cors-decorated",
        &function,
        Some(cors_configuration()),
        ROUTE_KEY,
        "NONE",
    )
    .await;

    for row in served_rows() {
        let case = row.case;
        let answered = sent(case, &api, row.method, ROUTE_PATH, row.headers).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected the published address to answer {}, got {answered:?}",
            row.status
        );
        match row.seen_by_the_function {
            Some(origin) => {
                let wanted = functions_answer(origin);
                assert_eq!(
                    answered.json(),
                    wanted,
                    "case {case:?}: expected the function's own answer {wanted}, got {answered:?}"
                );
                // A field of the backend's that is not an `Access-Control-*` one is the
                // backend's to state, so it reaches the client untouched.
                assert_eq!(
                    answered.field(BACKEND_FIELD),
                    Some(MARKER),
                    "case {case:?}: expected the function's own {BACKEND_FIELD} field, got \
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
        // Whatever the configuration states, neither value the FUNCTION chose for one of
        // these fields may ever reach a client of an api that carries one.
        for (name, value) in &carried {
            assert!(
                value != BACKEND_ALLOW_ORIGIN && value != BACKEND_EXPOSE_HEADERS,
                "case {case:?}: the api's configuration owns {name}, but the function's own \
                 value {value:?} was served"
            );
        }
    }
}

/// One row of the catch-all `AWS_IAM` api: what the request carries, and the decoration the
/// gateway's refusal of it must still be given.
struct RefusedRow {
    case: &'static str,
    method: &'static str,
    headers: &'static [(&'static str, &'static str)],
    decoration: fn() -> Vec<(String, String)>,
}

/// The rows a route that catches every method and path — a browser's preflight among them —
/// is reached with, by a caller carrying no AWS credential.
fn refused_rows() -> Vec<RefusedRow> {
    vec![
        RefusedRow {
            case: "an unsigned request carrying no browser field at all",
            method: ALLOWED_METHOD,
            headers: &[],
            decoration: ungranted,
        },
        RefusedRow {
            case: "an unsigned request from an origin the configuration admits",
            method: ALLOWED_METHOD,
            headers: &[("Origin", ALLOWED_ORIGIN), ("Sec-Fetch-Site", "cross-site")],
            decoration: granted,
        },
        RefusedRow {
            case: "an unsigned preflight the catch-all route matches",
            method: "OPTIONS",
            headers: &[
                ("Origin", ALLOWED_ORIGIN),
                ("Sec-Fetch-Site", "cross-site"),
                ("Access-Control-Request-Method", ALLOWED_METHOD),
                ("Access-Control-Request-Headers", ALLOWED_HEADERS),
            ],
            decoration: preflight_grant,
        },
        RefusedRow {
            case: "that preflight asking about a method the configuration does not admit",
            method: "OPTIONS",
            headers: &[
                ("Origin", ALLOWED_ORIGIN),
                ("Sec-Fetch-Site", "cross-site"),
                ("Access-Control-Request-Method", UNADMITTED_METHOD),
            ],
            decoration: ungranted,
        },
        RefusedRow {
            case: "that preflight from an origin the configuration does not admit",
            method: "OPTIONS",
            headers: &[
                ("Origin", FOREIGN_ORIGIN),
                ("Sec-Fetch-Site", "cross-site"),
                ("Access-Control-Request-Method", ALLOWED_METHOD),
            ],
            decoration: ungranted,
        },
    ]
}

/// RULE: a preflight a route of the api MATCHES is routed and authorized like any other
/// request, so an `AWS_IAM` route reached with no credential answers the gateway's own
/// `403 {"message":"Forbidden"}` — a JSON envelope, never a transport-level refusal — and the
/// api's configuration decorates that refusal exactly as it decorates a served answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_preflight_a_route_matches_is_authorized_and_refused_in_the_gateways_envelope() {
    let setup = "an api with a CorsConfiguration whose catch-all route is AWS_IAM";
    let (emulator, engine) = served_emulator();
    let function = given_function(
        &emulator,
        &engine,
        setup,
        "published-cors-iam-echo",
        REPORTS_THE_ORIGIN,
    )
    .await;
    let api = given_api(
        &emulator,
        setup,
        "published-cors-catch-all",
        &function,
        Some(cors_configuration()),
        CATCH_ALL_ROUTE_KEY,
        "AWS_IAM",
    )
    .await;

    for row in refused_rows() {
        let case = row.case;
        let answered = sent(case, &api, row.method, ROUTE_PATH, row.headers).await;
        assert_eq!(
            answered.status, FORBIDDEN,
            "case {case:?}: expected the route's authorization to refuse with {FORBIDDEN}, got \
             {answered:?}"
        );
        let wanted = json!({ "message": FORBIDDEN_MESSAGE });
        assert_eq!(
            answered.json(),
            wanted,
            "case {case:?}: expected the gateway's envelope {wanted}, got {answered:?}"
        );
        // The refusal is the route's authorization, made before its integration: nothing of
        // the function is in the answer.
        assert_eq!(
            answered.field(BACKEND_FIELD),
            None,
            "case {case:?}: a refused request must not have reached the function, got \
             {answered:?}"
        );
        let carried = decoration(&answered);
        let wanted = (row.decoration)();
        assert_eq!(
            carried, wanted,
            "case {case:?}: expected the CORS decoration {wanted:?}, got {carried:?}"
        );
    }

    // The other half of "routed like any other request": the same catch-all route key on an
    // api whose route admits every caller (`AuthorizationType` NONE, the CDK `$default`
    // catch-all's class) answers the preflight from its INTEGRATION — the function runs, sees
    // the `OPTIONS` request and the origin, and the configuration decorates its answer.
    let served_by_the_route = given_api(
        &emulator,
        setup,
        "published-cors-catch-all-unauthorized",
        &function,
        Some(cors_configuration()),
        CATCH_ALL_ROUTE_KEY,
        "NONE",
    )
    .await;
    let case = "a preflight the catch-all route of an unauthorized api matches";
    let answered = sent(
        case,
        &served_by_the_route,
        "OPTIONS",
        ROUTE_PATH,
        &[
            ("Origin", ALLOWED_ORIGIN),
            ("Sec-Fetch-Site", "cross-site"),
            ("Access-Control-Request-Method", ALLOWED_METHOD),
            ("Access-Control-Request-Headers", ALLOWED_HEADERS),
        ],
    )
    .await;
    assert_eq!(
        answered.status, SERVED,
        "case {case:?}: expected the matched route's integration to answer {SERVED}, got \
         {answered:?}"
    );
    let wanted = functions_answer(ALLOWED_ORIGIN);
    assert_eq!(
        answered.json(),
        wanted,
        "case {case:?}: expected the function's own answer {wanted}, got {answered:?}"
    );
    let carried = decoration(&answered);
    let wanted = preflight_grant();
    assert_eq!(
        carried, wanted,
        "case {case:?}: expected the CORS decoration {wanted:?}, got {carried:?}"
    );
}

/// RULE: `DeleteCorsConfiguration` takes the decoration — and the gateway's own answer to a
/// preflight — off the api's data plane without changing what it serves: the same
/// Origin-bearing request is still answered by the route's function, with no
/// `Access-Control-*` field at all, and a preflight no route matches goes back to being the
/// api's own `404 {"message":"Not Found"}`.
#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_configuration_undecorates_the_address_without_unserving_it() {
    let case = "the api's CorsConfiguration is deleted while its route keeps serving";
    let (emulator, engine) = served_emulator();
    let function = given_function(
        &emulator,
        &engine,
        case,
        "published-cors-deleted-echo",
        REPORTS_THE_ORIGIN_UNDECORATED,
    )
    .await;
    let api = given_api(
        &emulator,
        case,
        "published-cors-deleted",
        &function,
        Some(cors_configuration()),
        ROUTE_KEY,
        "NONE",
    )
    .await;

    let from_the_page: &[(&str, &str)] =
        &[("Origin", ALLOWED_ORIGIN), ("Sec-Fetch-Site", "cross-site")];
    let preflight: &[(&str, &str)] = &[
        ("Origin", ALLOWED_ORIGIN),
        ("Sec-Fetch-Site", "cross-site"),
        ("Access-Control-Request-Method", ALLOWED_METHOD),
        ("Access-Control-Request-Headers", ALLOWED_HEADERS),
    ];

    // While the configuration is there: the grant on the served answer, and the gateway's own
    // answer to the preflight.
    let before = sent(case, &api, ALLOWED_METHOD, ROUTE_PATH, from_the_page).await;
    assert_eq!(
        before.status, SERVED,
        "case {case:?}: expected the configured api to serve the request, got {before:?}"
    );
    assert_eq!(
        decoration(&before),
        granted(),
        "case {case:?}: expected the configuration's grant, got {before:?}"
    );
    let before_preflight = sent(case, &api, "OPTIONS", ROUTE_PATH, preflight).await;
    assert_eq!(
        before_preflight.status, PREFLIGHT,
        "case {case:?}: expected the gateway to answer the preflight itself, got \
         {before_preflight:?}"
    );
    assert_eq!(
        decoration(&before_preflight),
        preflight_grant(),
        "case {case:?}: expected the configuration's preflight grant, got {before_preflight:?}"
    );

    let (status, deleted) = control(
        &emulator,
        "DELETE",
        &format!("/v2/apis/{}/cors", api.api_id),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: DeleteCorsConfiguration answered {status} {deleted}"
    );
    let (status, fetched) = control(&emulator, "GET", &format!("/v2/apis/{}", api.api_id), None)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetApi answered {status} {fetched}"
    );
    assert_eq!(
        fetched.get("corsConfiguration"),
        None,
        "case {case:?}: GetApi must report no corsConfiguration once it is deleted, got {fetched}"
    );

    // And after it: the same request is served by the same function, with nothing decorating
    // it, and the preflight the gateway used to answer is a path the api has nothing at.
    let after = sent(case, &api, ALLOWED_METHOD, ROUTE_PATH, from_the_page).await;
    assert_eq!(
        after.status, SERVED,
        "case {case:?}: expected the request to stay served once the configuration is gone, got \
         {after:?}"
    );
    let wanted = functions_answer(ALLOWED_ORIGIN);
    assert_eq!(
        after.json(),
        wanted,
        "case {case:?}: expected the function's own answer {wanted}, got {after:?}"
    );
    assert_eq!(
        decoration(&after),
        ungranted(),
        "case {case:?}: expected no CORS field once the configuration is gone, got {after:?}"
    );
    let after_preflight = sent(case, &api, "OPTIONS", ROUTE_PATH, preflight).await;
    assert_eq!(
        after_preflight.status, NOT_FOUND,
        "case {case:?}: expected a preflight no route matches to be the api's own {NOT_FOUND}, \
         got {after_preflight:?}"
    );
    let wanted = json!({ "message": NOT_FOUND_MESSAGE });
    assert_eq!(
        after_preflight.json(),
        wanted,
        "case {case:?}: expected the gateway's envelope {wanted}, got {after_preflight:?}"
    );
    assert_eq!(
        decoration(&after_preflight),
        ungranted(),
        "case {case:?}: expected no CORS field once the configuration is gone, got \
         {after_preflight:?}"
    );
}
