// SPDX-License-Identifier: CC0-1.0

//! The restJson1 wire harness every MediaLive suite in this crate drives the
//! service through.
//!
//! One copy, so the suites cannot disagree about how a request reaches the
//! emulator: each test builds a real HTTP request and dispatches it through
//! `MultiServiceRouter`, so what is asserted is the emulator's wire behaviour,
//! not an internal function's return value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// The region [`call`] signs for, and so the region every ARN those requests
/// mint must name. A suite whose rule is scoped to a region signs for one of its
/// own with [`call_in_region`].
pub const DEFAULT_REGION: &str = "us-west-2";

/// A SigV4 credential scope naming `medialive`, which is how the router decides
/// this service claims the request, and `region`, which is the region the
/// request's resources live in.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/medialive/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// The account segment of every ARN a generated emulator emits
/// (`emulator_server::EMULATOR_ACCOUNT_ID`).
pub const ACCOUNT: &str = "000000000000";

/// A router carrying this service alone, on its own engine, so each test starts
/// from an empty store.
pub fn medialive() -> MultiServiceRouter {
    medialive_with_engine().0
}

/// The same router, handed back with the engine it was built on, for a suite
/// whose rule is about state no operation writes yet (a cluster's or a channel's
/// alert log, which the real service raises from hardware this emulator does not
/// run and media it does not ingest).
///
/// One construction path, so a suite that reaches the store cannot drift from
/// how every other suite builds the service.
pub fn medialive_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These suites exercise service behaviour, not request signing.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_medialive_medialive::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// Dispatch one request signed for [`DEFAULT_REGION`] and read back its status
/// and JSON body.
pub async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_in_region(router, DEFAULT_REGION, method, uri, body).await
}

/// Dispatch one request signed for `region` and read back its status and JSON
/// body.
pub async fn call_in_region(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, body, _headers) = call_with_headers(router, region, method, uri, body).await;
    (status, body)
}

/// Dispatch one request signed for `region` and read back its status, its JSON body
/// AND its response headers, for a suite whose rule is about a header.
///
/// [`call_in_region`] is this function with the headers dropped, and this one is
/// [`call_raw`] with the body parsed as JSON, so a suite that reads a header cannot
/// drift from how every other suite reaches the emulator.
pub async fn call_with_headers(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value, Vec<(String, String)>) {
    let (status, bytes, headers) = call_raw(router, region, method, uri, body, &[]).await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed, headers)
}

/// Dispatch one request signed for `region`, carrying `request_headers` beside the
/// ones every call sends, and read back its status, its RAW response body and its
/// response headers — for a suite whose rule is about a BINARY payload or about a
/// request header (the thumbnail read, whose response body is an image and whose
/// `Accept` says which media type it asks for).
///
/// The one place a request is built and dispatched in this crate:
/// [`call_with_headers`] is this function with the body parsed as JSON, so a suite
/// that reads raw bytes cannot drift from how every other suite reaches the
/// emulator.
pub async fn call_raw(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
    request_headers: &[(&str, &str)],
) -> (u16, Vec<u8>, Vec<(String, String)>) {
    let body = with_idempotency_token(method, uri, body);
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("medialive.{region}.amazonaws.com"))
        .header("authorization", authorization(region))
        .header("content-type", "application/json");
    for (name, value) in request_headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(payload).expect("build the request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    (status, response.into_body().to_vec(), headers)
}

/// The requests whose body carries an `@idempotencyToken` `RequestId` that
/// MediaLive requires: the creates of a channel, of an input, and of an input's
/// partner.
///
/// `path` is the URI path with no query string.
fn mints_from_a_request_id(method: &str, path: &str) -> bool {
    method == "POST"
        && (path == "/prod/channels"
            || path == "/prod/inputs"
            || (path.starts_with("/prod/inputs/") && path.ends_with("/partners")))
}

/// `body` with the idempotency token an AWS SDK would have put on it.
///
/// `RequestId` is `@idempotencyToken`, and an AWS SDK fills such a member with a
/// value of its own when the caller supplied none — so a client never sends one
/// of these creates without a token, and the harness sends them the way a client
/// does. Every suite therefore says only what ITS rule is about, and a suite
/// that wants a particular token still names it: a token the body already
/// carries, including an empty one, is left exactly as it is.
///
/// A suite whose rule IS the absent token says so with an explicit JSON `null`,
/// which is dropped here so the request truly carries no member.
fn with_idempotency_token(method: &str, uri: &str, body: Option<Value>) -> Option<Value> {
    let path = uri.split('?').next().unwrap_or(uri);
    if !mints_from_a_request_id(method, path) {
        return body;
    }
    let mut object = match body {
        Some(Value::Object(members)) => members,
        // A create of one of these resources always carries a body, because the
        // token travels in it.
        None => serde_json::Map::new(),
        other => return other,
    };
    match object.get("requestId") {
        Some(Value::Null) => {
            object.remove("requestId");
        }
        Some(_) => {}
        None => {
            object.insert("requestId".to_string(), Value::from(fresh_token()));
        }
    }
    Some(Value::Object(object))
}

/// A token no other request of this process uses, so two creates the harness
/// filled are never read as a retry of one another.
fn fresh_token() -> String {
    static MINTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let minted = MINTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("harness-token-{minted}")
}

/// One response header, read as a client reads it (header names are
/// case-insensitive, so the lookup is too).
pub fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// The `message` member of a restJson1 error body (`jsonName: "message"` on
/// every one of this service's error shapes), as a client reads it. One copy for
/// every suite, beside [`error_code`].
pub fn error_message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code a restJson1 error response carries, as a client reads it.
pub fn error_code(status: u16, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map_or_else(|| format!("<no __type, status {status}>"), ToString::to_string)
}
