// SPDX-License-Identifier: CC0-1.0

//! The CORS preflight on a GraphQL API's `GRAPHQL` endpoint, and what a page may READ of
//! what that endpoint answers — driven over the API's own published socket.
//!
//! The rule these rows assert, one level above the recorded case (one OPTIONS carrying an
//! `Origin` and the method and fields a browser was about to send):
//!
//! > A GraphQL API's `GRAPHQL` endpoint is an address a PAGE calls, so it answers the CORS
//! > preflight itself: an `OPTIONS` carrying `Access-Control-Request-Method` is answered
//! > `2xx` with no body and with no authorization material of any kind, since a browser sends
//! > none on a preflight, and it states what the real request may carry — the method and the
//! > fields the browser asked about, how long the answer may be reused, the permitted origin
//! > and the response fields a script may read. The REAL request of a page served from THIS
//! > MACHINE reaches the API and is decided by the material it presents like any other call;
//! > a page of any other site is refused `403` before anything of the API is reached, unless
//! > the operator asked for that site. Every answer the endpoint's own route produced — the
//! > result and each refusal — states the origin it permits and the fields a script may read,
//! > which is what lets the page read the GraphQL error envelope at all, and an answer to a
//! > request that reached no operation of the API states neither.
//!
//! The rows span what the rule generalises over: the method the browser asks about, the
//! fields it asks for (several, one, none at all), the origin it declares (an ordinary site, a
//! page served from a loopback port, another loopback address, the opaque `null` of a
//! sandboxed frame), whether the preflight presents an API key, and — for the real request —
//! the site the page was served from and whether the material it presents authorizes it, since
//! both answers have to be readable from script.
//!
//! The real request of a page from a site the operator has NOT asked for is refused here,
//! which is why the preflight's own answer states only what this endpoint permits to carry
//! rather than that any page will be served: `APPSYNC_GRAPHQL_REMOTE_ORIGINS` is what admits
//! such a site.

use serde_json::{json, Value};

use crate::graphql_api_mapping_template_resolvers::{
    create_api, emulator, try_post_answer, GraphqlApi, GRAPHQL_PATH,
};

/// The fields a browser asks a preflight with, and the ones the answer states the permission
/// in (Fetch, §3.2.2 and §3.2.3). The service's own copies are
/// `service::graphql_api::query::REQUEST_METHOD_FIELD` and its siblings.
const REQUEST_METHOD_FIELD: &str = "access-control-request-method";
const REQUEST_HEADERS_FIELD: &str = "access-control-request-headers";
const ALLOW_ORIGIN_FIELD: &str = "access-control-allow-origin";
const ALLOW_METHODS_FIELD: &str = "access-control-allow-methods";
const ALLOW_HEADERS_FIELD: &str = "access-control-allow-headers";
const EXPOSE_HEADERS_FIELD: &str = "access-control-expose-headers";
const MAX_AGE_FIELD: &str = "access-control-max-age";

/// The response fields the endpoint states a page may read; the service's own copy is
/// `service::graphql_api::query::EXPOSED_FIELDS`. They are the two fields the endpoint writes
/// ABOUT an answer: the request's own id and the class of a refusal.
const EXPOSED_FIELDS: [&str; 2] = ["x-amzn-requestid", "x-amzn-errortype"];

/// How long a browser may reuse the preflight answer; the service's own copy is
/// `service::graphql_api::query::PREFLIGHT_MAX_AGE_SECONDS`.
const PREFLIGHT_MAX_AGE: &str = "172800";

/// The origin the endpoint permits where it names no single one; the service's own copy is
/// `service::graphql_api::query::ALLOWED_ORIGIN`.
const ALLOWED_ORIGIN: &str = "*";

/// What a preflight permits when the browser asked about no field in particular; the service's
/// own copy is `service::graphql_api::query::EVERY_FIELD`.
const EVERY_FIELD: &str = "*";

/// The field an answer names what its own value depends on in.
const VARY_FIELD: &str = "vary";

/// The field an API key travels in.
const API_KEY_FIELD: &str = "x-api-key";

/// The error type every unauthorized call on this data plane earns; the service's own copy is
/// `service::event_api::UNAUTHORIZED_ERROR_TYPE`.
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";

#[tokio::test]
async fn the_graphql_endpoint_answers_the_cors_preflight_unauthenticated() {
    let router = emulator();
    let api = create_api(&router, "preflight", None).await;

    /// One row: what the browser asks the preflight with, whether it presents a key, and the
    /// origin the answer must permit — the page's own where it was served from this machine,
    /// and any origin for a site the answer grants nothing else to.
    struct Row {
        case: &'static str,
        origin: &'static str,
        method: &'static str,
        asked_headers: Option<&'static str>,
        with_key: bool,
        permitted: &'static str,
    }
    let rows: &[Row] = &[
        Row {
            case: "the fields a GraphQL client sends, from an ordinary site",
            origin: "https://example.com",
            method: "POST",
            asked_headers: Some("content-type,x-api-key"),
            with_key: false,
            permitted: ALLOWED_ORIGIN,
        },
        Row {
            case: "one field asked about, from a page on a loopback port",
            origin: "http://localhost:5173",
            method: "POST",
            asked_headers: Some("authorization"),
            with_key: false,
            permitted: "http://localhost:5173",
        },
        Row {
            case: "no fields asked about at all",
            origin: "https://example.com",
            method: "POST",
            asked_headers: None,
            with_key: false,
            permitted: ALLOWED_ORIGIN,
        },
        Row {
            case: "another method asked about, from another loopback address",
            origin: "http://127.0.0.1:3000",
            method: "GET",
            asked_headers: Some("content-type"),
            with_key: false,
            permitted: "http://127.0.0.1:3000",
        },
        Row {
            case: "the opaque origin of a sandboxed frame",
            origin: "null",
            method: "POST",
            asked_headers: Some("content-type,x-api-key"),
            with_key: false,
            permitted: ALLOWED_ORIGIN,
        },
        Row {
            case: "a preflight that does present the api's key",
            origin: "https://example.com",
            method: "POST",
            asked_headers: Some("content-type,x-api-key"),
            with_key: true,
            permitted: ALLOWED_ORIGIN,
        },
    ];

    for row in rows {
        let case = row.case;
        let mut fields: Vec<(&str, &str)> = vec![
            ("origin", row.origin),
            ("sec-fetch-mode", "cors"),
            (REQUEST_METHOD_FIELD, row.method),
        ];
        if let Some(asked) = row.asked_headers {
            fields.push((REQUEST_HEADERS_FIELD, asked));
        }
        if row.with_key {
            fields.push((API_KEY_FIELD, api.api_key.as_str()));
        }
        let (status, answered, body) = answer(case, &api, "OPTIONS", &json!({}), &fields).await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: the CORS preflight must succeed, got {status} and {body}"
        );
        assert_eq!(
            body,
            Value::Null,
            "case {case:?}: a preflight answer carries no body, got {body}"
        );
        assert_eq!(
            field(&answered, ALLOW_ORIGIN_FIELD).as_deref(),
            Some(row.permitted),
            "case {case:?}: expected the preflight to permit {:?}, got {answered:?}",
            row.permitted
        );
        assert_eq!(
            field(&answered, ALLOW_METHODS_FIELD).as_deref(),
            Some(row.method),
            "case {case:?}: the preflight must permit the method it was asked about, got \
             {answered:?}"
        );
        let permitted_headers = field(&answered, ALLOW_HEADERS_FIELD);
        assert_eq!(
            permitted_headers.as_deref(),
            Some(row.asked_headers.unwrap_or(EVERY_FIELD)),
            "case {case:?}: the preflight must permit the fields it was asked about, got \
             {answered:?}"
        );
        assert_eq!(
            field(&answered, MAX_AGE_FIELD).as_deref(),
            Some(PREFLIGHT_MAX_AGE),
            "case {case:?}: the preflight must state how long it may be cached, got \
             {answered:?}"
        );
        assert!(
            exposes(&answered),
            "case {case:?}: the preflight must expose the fields the endpoint writes about an \
             answer, got {answered:?}"
        );
        assert_eq!(
            field(&answered, VARY_FIELD).as_deref().map(str::to_ascii_lowercase),
            Some("origin".to_string()),
            "case {case:?}: an answer whose permitted origin depends on the request's own must \
             say so, got {answered:?}"
        );
    }

    // An `OPTIONS` that asks about no method is no preflight, and the preflight is the only
    // `OPTIONS` this endpoint routes, so it addresses nothing this endpoint serves.
    let case = "an OPTIONS that is not a preflight";
    let (status, answered, body) = answer(
        case,
        &api,
        "OPTIONS",
        &json!({}),
        &[("origin", "http://localhost:5173")],
    )
    .await;
    assert_eq!(
        (status, body.pointer("/errors/0/errorType").and_then(Value::as_str)),
        (404, Some("UnknownOperationException")),
        "case {case:?}: expected the refusal of a request that reaches no operation of the \
         API, got {status} and {body}"
    );
    assert_eq!(
        field(&answered, ALLOW_ORIGIN_FIELD),
        None,
        "case {case:?}: a request that reaches no operation of the API is granted no \
         cross-origin access, got {answered:?}"
    );
}

#[tokio::test]
async fn a_browser_request_is_decided_by_the_site_it_came_from_and_its_answer_is_readable() {
    let router = emulator();
    let api = create_api(&router, "readable", None).await;
    let live = api.api_key.clone();

    /// One row: the site the page was served from and the material it presents, and the
    /// answer it must get — a page served from THIS MACHINE reaches the API and is decided by
    /// its material, and a page of any other site reaches nothing of it.
    struct Row {
        case: &'static str,
        origin: &'static str,
        api_key: Option<&'static str>,
        status: u16,
        /// The `errorType` the answer must carry, `None` for a request that is served.
        error_type: Option<&'static str>,
        /// The origin the answer must permit the page to read it from.
        permitted: &'static str,
    }
    let rows: &[Row] = &[
        Row {
            case: "a page on this machine presenting the api's own key",
            origin: "http://localhost:5173",
            api_key: Some(LIVE_KEY),
            status: 200,
            error_type: None,
            permitted: "http://localhost:5173",
        },
        Row {
            case: "a page on this machine presenting no key at all",
            origin: "http://localhost:5173",
            api_key: None,
            status: 401,
            error_type: Some(UNAUTHORIZED_ERROR_TYPE),
            permitted: "http://localhost:5173",
        },
        Row {
            case: "a page on this machine presenting a key this API never minted",
            origin: "http://127.0.0.1:3000",
            api_key: Some("da2-neverminted000000000000"),
            status: 401,
            error_type: Some(UNAUTHORIZED_ERROR_TYPE),
            permitted: "http://127.0.0.1:3000",
        },
        Row {
            case: "a page of another site, presenting the api's own key",
            origin: "https://attacker.example",
            api_key: Some(LIVE_KEY),
            status: 403,
            error_type: Some(UNAUTHORIZED_ERROR_TYPE),
            permitted: ALLOWED_ORIGIN,
        },
        Row {
            case: "a page of another site, presenting no key",
            origin: "https://attacker.example",
            api_key: None,
            status: 403,
            error_type: Some(UNAUTHORIZED_ERROR_TYPE),
            permitted: ALLOWED_ORIGIN,
        },
        Row {
            case: "the opaque origin of a sandboxed frame",
            origin: "null",
            api_key: Some(LIVE_KEY),
            status: 403,
            error_type: Some(UNAUTHORIZED_ERROR_TYPE),
            permitted: ALLOWED_ORIGIN,
        },
    ];

    for row in rows {
        let case = row.case;
        let key = match row.api_key {
            Some(LIVE_KEY) => live.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let mut fields: Vec<(&str, &str)> = vec![
            ("origin", row.origin),
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "cors"),
        ];
        if row.api_key.is_some() {
            fields.push((API_KEY_FIELD, key.as_str()));
        }
        let (status, answered, body) = answer(
            case,
            &api,
            "POST",
            &json!({"query": "query { probe(msg: \"cors\") }"}),
            &fields,
        )
        .await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected the status {}, got {status} and {body}",
            row.status
        );
        assert_eq!(
            body.pointer("/errors/0/errorType").and_then(Value::as_str),
            row.error_type,
            "case {case:?}: expected the errorType {:?}, got {body}",
            row.error_type
        );
        match row.error_type {
            None => assert_eq!(
                body["data"]["probe"].as_str(),
                Some("cors"),
                "case {case:?}: a page this endpoint serves reaches the resolver, got {body}"
            ),
            Some(_) => assert!(
                body.get("data").is_none(),
                "case {case:?}: a refused request reaches no resolver, got {body}"
            ),
        }
        // Whichever it is, the page that made the request may read the answer.
        assert_eq!(
            field(&answered, ALLOW_ORIGIN_FIELD).as_deref(),
            Some(row.permitted),
            "case {case:?}: expected the answer to permit {:?}, got {answered:?} and {body}",
            row.permitted
        );
        assert!(
            exposes(&answered),
            "case {case:?}: every answer must expose the fields the endpoint writes about it, \
             got {answered:?} and {body}"
        );
        assert_eq!(
            field(&answered, VARY_FIELD).as_deref().map(str::to_ascii_lowercase),
            Some("origin".to_string()),
            "case {case:?}: an answer whose permitted origin depends on the request's own must \
             say so, got {answered:?} and {body}"
        );
    }

    // And a client that is no page at all — no `Origin`, no `Sec-Fetch-*` — is served as it
    // always was, with the answer readable by any page that is handed it.
    let case = "a client that is no page at all";
    let (status, answered, body) = answer(
        case,
        &api,
        "POST",
        &json!({"query": "query { probe(msg: \"cors\") }"}),
        &[(API_KEY_FIELD, live.as_str())],
    )
    .await;
    assert_eq!(
        (status, body["data"]["probe"].as_str()),
        (200, Some("cors")),
        "case {case:?}: expected the resolver's own answer, got {status} and {body}"
    );
    assert_eq!(
        field(&answered, ALLOW_ORIGIN_FIELD).as_deref(),
        Some(ALLOWED_ORIGIN),
        "case {case:?}: expected the answer to permit any origin, got {answered:?}"
    );
}

/// The stand-in a row uses for "the API's own live key", which is minted per run.
const LIVE_KEY: &str = "<the api's own key>";

/// Address the API's published endpoint with `method` and the request `fields`, answering the
/// status, the response fields and the document.
async fn answer(
    case: &str,
    api: &GraphqlApi,
    method: &str,
    body: &Value,
    fields: &[(&str, &str)],
) -> (u16, Vec<(String, String)>, Value) {
    try_post_answer(case, &api.graphql, GRAPHQL_PATH, method, body, fields)
        .await
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the published GraphQL endpoint {:?} must answer this request",
                api.graphql
            )
        })
}

/// Whether the answer states that a page may read every field the endpoint writes ABOUT it.
fn exposes(fields: &[(String, String)]) -> bool {
    let exposed = field(fields, EXPOSE_HEADERS_FIELD).unwrap_or_default();
    let named: Vec<String> = exposed
        .split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .collect();
    EXPOSED_FIELDS
        .iter()
        .all(|wanted| named.iter().any(|named| named == wanted))
}

/// The value of one response field, read as a client reads it: field names are
/// case-insensitive, and the reader lowercases them.
fn field(fields: &[(String, String)], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value.clone())
}
