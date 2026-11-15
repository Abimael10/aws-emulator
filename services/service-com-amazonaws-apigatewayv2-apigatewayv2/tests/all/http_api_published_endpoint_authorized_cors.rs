// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an HTTP api's PUBLISHED `ApiEndpoint` serves: the api's own
//! `CorsConfiguration`-decorated, `AWS_IAM`-authorized route, reached at the invoke URL a
//! client builds out of that published value and nothing else.
//!
//! The rule, one level above the recorded case (an HTTP api created with a
//! `CorsConfiguration` through a client configured with this emulator's own endpoint, whose
//! published `apiEndpoint` named a host that client resolves nowhere, so the signed invoke
//! of its `AWS_IAM` route was never sent at all):
//!
//! > `ApiEndpoint` is the only address a caller is given for an api's data plane — "the
//! > stage name is typically appended to this URI to form a complete path to a deployed API
//! > stage" (`Api$ApiEndpoint`) — so every operation that reports an api publishes an
//! > address the client that read it can DIAL and this emulator answers that api's stages
//! > on, and the api's own records then decide a request arriving there exactly as they
//! > decide one arriving at any other address the api is reached at: the route it matches,
//! > what that route's `AuthorizationType` requires of it, and the `Access-Control-*`
//! > decoration the api's `CorsConfiguration` owns.
//!
//! The rows span what the rule generalises over: the endpoint the control-plane client was
//! configured with (an IPv4 loopback literal with a port, a loopback NAME on another port, a
//! bracketed IPv6 literal, and one of AWS's own hosts, which is the client that resolves
//! them), the Region the control-plane request is signed for (including the emulator's own
//! default, so a row cannot pass on a hardcoded Region), HOW the invoke URL is composed out
//! of the published value (the endpoint on its own, for the stage that serves the api's root,
//! and that endpoint with the stage name appended) and WHICH request is sent (a signed GET
//! from the allow-listed origin, the same GET carrying no credential at all, the preflight a
//! browser sends ahead of it, and a signed GET from an origin the configuration does not
//! admit). The recorded case is ONE row.
//!
//! The FORM of the published value is `http_api_published_invoke_address`'s rule, and the
//! decoration's own table — every `Cors` member, the wildcard, an api carrying no
//! configuration — is `http_api_cors_data_plane`'s. This suite holds the composition the
//! recorded check performs and neither of those covers: the value the CONTROL PLANE
//! published is the address the AUTHORIZED, DECORATED data plane answers on.
//!
//! Request authentication is genuinely ON for every row — the control asserts a modelled
//! control-plane operation is refused by the same emulator — so the signed case and the
//! unsigned case differ in what they carry, and not only in what they expect. What a route's
//! `AuthorizationType` decides about an invoke is the PRESENCE of a SigV4 credential and not
//! its signature (`crate::state` documents the same: an invoke is signed under the data
//! plane's own signing name, and a service never verifies a signature itself), so a signed
//! row here is evidence of that and of nothing more.
//!
//! Every request is dispatched IN PROCESS, against a `MultiServiceRouter` this suite builds
//! (`crate::api_lifecycle::emulator`): the host strings below are `Host` header values handed
//! to that dispatcher, so nothing here binds a socket, opens a listener or mounts an endpoint
//! — which address this emulator SERVES is decided by `crate::service::invoke`, unchanged by
//! this suite.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call_at_host, emulator, error_code};
use crate::websocket_handshake_data_plane::{message, send, Answer};

/// The status an `HTTP_PROXY` route answers: the upstream is a URI the CALLER can reach, so
/// the caller is sent there — `307`, with the integration's own `IntegrationUri` in
/// `Location`. Reaching it proves the request resolved all the way to the route's
/// integration, which is what a published address being SERVED means.
const SERVED: u16 = 307;

/// The status and sentence an `AWS_IAM` route reached with no AWS credential earns.
const FORBIDDEN: u16 = 403;
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The status the GATEWAY answers a preflight no route of the api matches: the api's one
/// route catches the actual `GET` only, so the configuration's own headers are the whole
/// answer.
const PREFLIGHT: u16 = 204;

/// The front door's envelope for a request no service claimed. A published address nothing
/// serves is the defect this suite pins, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The code the emulator's own credential gate refuses an unverifiable request with. It is
/// the gate's, not AWS's: the control asserts it on a MODELLED operation, which is where it
/// belongs.
const CREDENTIAL_GATE_CODE: &str = "AccessDenied";

/// The upstream every row's `HTTP_PROXY` integration fronts.
const UPSTREAM: &str = "https://upstream.example.com/items";

/// The route every row deploys, the path within the stage that matches it, and the method it
/// admits.
const ROUTE_KEY: &str = "GET /items";
const ROUTE_PATH: &str = "/items";
const ALLOWED_METHOD: &str = "GET";

/// The reserved stage "that serves an api's root", reached with no stage segment at all.
const DEFAULT_STAGE: &str = "$default";

/// The prefix of every response header the decoration is made of, so an answer can be
/// asserted to carry EXACTLY what the configuration states and nothing else.
const CORS_HEADER_PREFIX: &str = "access-control-";

/// The origin the api's `CorsConfiguration` admits, and one it does not.
const ALLOWED_ORIGIN: &str = "https://app.example.com";
const REFUSED_ORIGIN: &str = "https://other.example.test";

/// The `AllowHeaders` the configuration names, as the preflight's answer joins them.
const ALLOWED_HEADERS: &str = "authorization,x-amz-date";

/// The `ExposeHeaders` the configuration names, as an actual request's answer joins them.
const EXPOSED_HEADERS: &str = "x-app-marker";

/// The signing name an invoke of a deployed stage is signed under: the `execute-api` service,
/// which AWS publishes the invoke endpoint and the `execute-api:Invoke` action under. NOT
/// this service's own signing name, which is its control plane's.
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// A value in the shape of a SigV4 signature: 64 lowercase hex characters.
const SIGNATURE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The `x-amz-date` a header-signed request carries, and the date part of its scope.
const AMZ_DATE: &str = "20261021T000000Z";
const SCOPE_DATE: &str = "20261021";

/// The `Authorization` header an invoke signed for `region` under the data plane's own
/// signing name carries.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/{SCOPE_DATE}/{region}/{INVOKE_SIGNING_NAME}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature={SIGNATURE}"
    )
}

/// The configuration every row's api carries: the one allow-listed origin and method the
/// signed invoke comes from, the headers that invoke's credential travels in, and one header
/// the answer exposes to the page.
fn cors_configuration() -> Value {
    json!({
        "allowOrigins": [ALLOWED_ORIGIN],
        "allowMethods": [ALLOWED_METHOD],
        "allowHeaders": ["authorization", "x-amz-date"],
        "exposeHeaders": [EXPOSED_HEADERS]
    })
}

/// One row: the endpoint the control-plane calls are addressed at, the Region they are
/// signed for, and whether that client resolves nothing but the endpoint it reached — which
/// is what decides the address the api is published on.
struct Row {
    case: &'static str,
    /// The `Host` the control-plane request carries — the endpoint the client is configured
    /// with.
    host: &'static str,
    region: &'static str,
    reaches_only_the_emulator: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: a client configured with the emulator's loopback \
                   endpoint",
            host: "127.0.0.1:35027",
            region: "us-west-2",
            reaches_only_the_emulator: true,
        },
        Row {
            case: "the same endpoint named by its loopback NAME on another port",
            host: "localhost:9297",
            region: "eu-west-1",
            reaches_only_the_emulator: true,
        },
        Row {
            case: "the same endpoint named by a bracketed IPv6 literal",
            host: "[::1]:9297",
            region: "ap-southeast-2",
            reaches_only_the_emulator: true,
        },
        Row {
            case: "the emulator's own default Region, signed for explicitly",
            host: "127.0.0.1:9297",
            region: "us-east-1",
            reaches_only_the_emulator: true,
        },
        Row {
            case: "a client that resolves AWS's own hosts: this service's own endpoint host",
            host: "apigateway.eu-north-1.amazonaws.com",
            region: "eu-north-1",
            reaches_only_the_emulator: false,
        },
    ]
}

/// Create the row's api — the `CorsConfiguration` above, an `HTTP_PROXY` integration, the one
/// `AWS_IAM` route that fronts it and an auto-deploying root stage — and answer its `ApiId`
/// and the `ApiEndpoint` `CreateApi` published.
async fn deploy(emulator: &MultiServiceRouter, row: &Row, name: &str) -> (String, String) {
    let case = row.case;
    let (status, api) = call_at_host(
        emulator,
        row.host,
        row.region,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": name,
            "protocolType": "HTTP",
            "corsConfiguration": cors_configuration()
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {status} {api}"
    );
    let api_id = as_str(&api, "apiId");
    let published = as_str(&api, "apiEndpoint");

    let (status, integration) = call_at_host(
        emulator,
        row.host,
        row.region,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "HTTP_PROXY",
            "integrationMethod": ALLOWED_METHOD,
            "integrationUri": UPSTREAM,
            "payloadFormatVersion": "1.0"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {status} {integration}"
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
            "target": format!("integrations/{integration_id}"),
            "authorizationType": "AWS_IAM"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {status} {route}"
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
        "case {case:?}: CreateStage must answer 201, got {status} {stage}"
    );
    assert!(
        stage.get("deploymentId").and_then(Value::as_str).is_some(),
        "case {case:?}: an auto-deploying stage must carry a deployment, got {stage}"
    );

    (api_id, published)
}

/// Assert `published` is an address the row's client can dial, and answer the authority it
/// dials and the base path it carries ahead of the stage.
///
/// A client that resolves nothing but the emulator's own endpoint is published that endpoint
/// with the api behind the reserved data-plane path; a client that resolves AWS's own
/// hostnames is published the api's own documented `execute-api` host, which this emulator's
/// front door claims as well.
fn assert_published(row: &Row, reporter: &str, api_id: &str, published: &str) -> (String, String) {
    let case = row.case;
    let rest = published.strip_prefix("https://").unwrap_or_else(|| {
        panic!("case {case:?}: {reporter} must publish an https endpoint, got {published:?}")
    });
    let (authority, base_path) = match rest.find('/') {
        Some(at) => (rest[..at].to_string(), rest[at..].to_string()),
        None => (rest.to_string(), String::new()),
    };
    let (expected_authority, expected_base_path) = if row.reaches_only_the_emulator {
        (row.host.to_string(), format!("/_aws/execute-api/{api_id}"))
    } else {
        (
            format!("{api_id}.execute-api.{}.amazonaws.com", row.region),
            String::new(),
        )
    };
    assert_eq!(
        (authority.as_str(), base_path.as_str()),
        (expected_authority.as_str(), expected_base_path.as_str()),
        "case {case:?}: {reporter} must publish the api at \
         {expected_authority}{expected_base_path}, got {published:?}"
    );
    (authority, base_path)
}

/// Assert the emulator really does verify credentials: a modelled control-plane operation
/// carrying the harness's unverifiable credential is refused by it, with the status and code
/// the credential gate answers — not merely with some non-success a routing failure would
/// also produce.
async fn assert_authenticating(emulator: &MultiServiceRouter, row: &Row) {
    let (status, body) = call_at_host(emulator, row.host, row.region, "GET", "/v2/apis", None).await;
    assert_eq!(
        (status, error_code(&body)),
        (403, CREDENTIAL_GATE_CODE.to_string()),
        "case {:?}: the control: request authentication must be on, so a modelled operation \
         earns the credential gate's 403 {CREDENTIAL_GATE_CODE}, got {status} {body}",
        row.case
    );
}

/// Every `Access-Control-*` header one answer carries, sorted, so a case asserts the whole
/// decoration rather than a subset of it.
fn decoration(answer: &Answer) -> Vec<(String, String)> {
    let mut carried: Vec<(String, String)> = answer
        .headers
        .iter()
        .filter(|(name, _)| name.as_str().starts_with(CORS_HEADER_PREFIX))
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).to_string(),
            )
        })
        .collect();
    carried.sort();
    carried
}

/// `pairs` as a sorted decoration, for a case to state what the answer must carry.
fn expected(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    owned.sort();
    owned
}

/// The decoration an ACTUAL request from the allow-listed origin earns: that origin echoed
/// back and the headers the configuration exposes.
fn actual_request_decoration() -> Vec<(String, String)> {
    expected(&[
        ("access-control-allow-origin", ALLOWED_ORIGIN),
        ("access-control-expose-headers", EXPOSED_HEADERS),
    ])
}

/// The decoration a PREFLIGHT from that origin earns: the origin, and the methods and
/// headers the configuration allows.
fn preflight_decoration() -> Vec<(String, String)> {
    expected(&[
        ("access-control-allow-origin", ALLOWED_ORIGIN),
        ("access-control-allow-methods", ALLOWED_METHOD),
        ("access-control-allow-headers", ALLOWED_HEADERS),
    ])
}

/// One request sent to the published address, and the answer the api's own records earn it.
struct Case<'a> {
    what: &'static str,
    method: &'static str,
    /// What the client sends beside the `Host`: its credential, and the CORS request fields
    /// a browser sets.
    headers: Vec<(&'a str, &'a str)>,
    status: u16,
    /// The sentence under the answer's `message` member, for a refusal the gateway worded.
    sentence: Option<&'static str>,
    /// The upstream the answer names, for a request the route's integration served.
    upstream: Option<&'static str>,
    decoration: Vec<(String, String)>,
}

/// The requests every form of the published invoke URL is driven with, `signed` carrying the
/// credential of the row's Region.
fn cases<'a>(signed: &'a str) -> Vec<Case<'a>> {
    vec![
        Case {
            what: "a signed GET from the allow-listed origin",
            method: ALLOWED_METHOD,
            headers: vec![
                ("authorization", signed),
                ("x-amz-date", AMZ_DATE),
                ("origin", ALLOWED_ORIGIN),
            ],
            status: SERVED,
            sentence: None,
            upstream: Some(UPSTREAM),
            decoration: actual_request_decoration(),
        },
        Case {
            what: "the same GET carrying no credential at all",
            method: ALLOWED_METHOD,
            headers: vec![("origin", ALLOWED_ORIGIN)],
            status: FORBIDDEN,
            sentence: Some(FORBIDDEN_MESSAGE),
            upstream: None,
            decoration: actual_request_decoration(),
        },
        Case {
            what: "the preflight a browser sends ahead of that GET",
            method: "OPTIONS",
            headers: vec![
                ("origin", ALLOWED_ORIGIN),
                ("access-control-request-method", ALLOWED_METHOD),
                ("access-control-request-headers", ALLOWED_HEADERS),
            ],
            status: PREFLIGHT,
            sentence: None,
            upstream: None,
            decoration: preflight_decoration(),
        },
        Case {
            what: "a signed GET from an origin the configuration does not admit",
            method: ALLOWED_METHOD,
            headers: vec![
                ("authorization", signed),
                ("x-amz-date", AMZ_DATE),
                ("origin", REFUSED_ORIGIN),
            ],
            status: SERVED,
            sentence: None,
            upstream: Some(UPSTREAM),
            decoration: Vec::new(),
        },
    ]
}

/// The address `CreateApi` and `GetApi` publish is the one the api's `AWS_IAM` route and its
/// CORS decoration are reached at, in each form a client builds the invoke URL in.
#[tokio::test]
async fn the_published_endpoint_serves_the_api_s_authorized_cors_configured_route() {
    for (index, row) in rows().into_iter().enumerate() {
        let mut emulator = emulator();
        let case = row.case;
        let (api_id, published) =
            deploy(&emulator, &row, &format!("published-cors-iam-{index}")).await;
        let (authority, base_path) = assert_published(&row, "CreateApi", &api_id, &published);

        // The two operations the recorded client reads the address from publish one value.
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
        let reported = as_str(&fetched, "apiEndpoint");
        assert_published(&row, "GetApi", &api_id, &reported);
        assert_eq!(
            reported, published,
            "case {case:?}: GetApi must publish the address CreateApi did, got {fetched}"
        );

        // From here on the SAME emulator verifies request signatures, as it does when it is
        // started with request authentication on.
        emulator.set_request_authentication(true);
        assert_authenticating(&emulator, &row).await;

        let signed = authorization(row.region);
        let forms = [
            (
                "the endpoint on its own, for the stage that serves the api's root",
                base_path.clone(),
            ),
            (
                "the endpoint with the stage name appended",
                format!("{base_path}/{DEFAULT_STAGE}"),
            ),
        ];
        for (form, stage_base) in &forms {
            let path = format!("{stage_base}{ROUTE_PATH}");
            for expectation in cases(&signed) {
                let what = format!(
                    "case {case:?}, {form} ({authority}{path}), {}",
                    expectation.what
                );
                let answer = send(
                    &emulator,
                    &authority,
                    expectation.method,
                    &path,
                    &expectation.headers,
                )
                .await;
                assert_ne!(
                    answer.error_type.as_deref(),
                    Some(UNKNOWN_SERVICE),
                    "{what}: the published address must be one this emulator serves, but it is \
                     claimed by no service: {}",
                    answer.summary()
                );
                assert_eq!(
                    answer.status,
                    expectation.status,
                    "{what}: expected {}, got {}",
                    expectation.status,
                    answer.summary()
                );
                if let Some(sentence) = expectation.sentence {
                    assert_eq!(
                        message(&answer),
                        sentence,
                        "{what}: expected the sentence {sentence:?}, got {}",
                        answer.summary()
                    );
                }
                assert_eq!(
                    answer.location.as_deref(),
                    expectation.upstream,
                    "{what}: expected the upstream {:?}, got {:?}",
                    expectation.upstream,
                    answer.location
                );
                let carried = decoration(&answer);
                assert_eq!(
                    carried, expectation.decoration,
                    "{what}: expected the CORS headers {:?}, got {carried:?}",
                    expectation.decoration
                );
            }
        }
    }
}
