// SPDX-License-Identifier: CC0-1.0

//! Shared harness for this crate's suites: one copy of the router, the request
//! builder and the response readers.
//!
//! Declared in `tests/all.rs` like any other suite (the layout rule requires every
//! file under `tests/all/` to be declared), so the sibling suites reach it as
//! `crate::support`. It holds no tests of its own — only the pieces
//! `connection_lifecycle` and `probe` would otherwise each own a copy of, which would
//! then drift.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use bytes::Bytes;
use engine::InMemoryEngine;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

/// The endpoint host an `execute-api` caller addresses. Its first label is also the
/// SigV4 signing name this service claims, so a request routes to it either way.
pub const HOST: &str = "execute-api.us-east-1.amazonaws.com";

/// A router serving only this service over `engine`. Built per call on purpose: every
/// router shares the one engine, so anything a later call observes came from engine
/// state and not from a handle held across calls.
fn router(engine: &Arc<InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(mgmt::EmulatedService::new(Arc::clone(engine)));
    router
}

/// A signed-looking request addressing `path` on `host`, presenting `access_key_id` as its
/// SigV4 credential.
fn request(
    access_key_id: &str,
    host: &str,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> http::Request<Bytes> {
    http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/us-east-1/execute-api/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(Bytes::from(body))
        .expect("the test request is well formed")
}

/// The access key id a request presents unless a case names another: one this emulator holds
/// no record for, which is the position the account's own published credential is in.
pub const DEFAULT_ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// Dispatch one request through the production router and return its status and body.
pub async fn call(
    engine: &Arc<InMemoryEngine>,
    method: &str,
    connection_id: &str,
    body: Vec<u8>,
) -> (u16, Bytes) {
    call_at(engine, HOST, method, &format!("/@connections/{connection_id}"), body).await
}

/// Dispatch one request at an arbitrary endpoint host and request path — what a caller
/// addressing the owning API's invoke URL sends, where the path carries that URL's own
/// prefix (its stage, or a custom domain's base-path mapping) in front of the modelled
/// `/@connections/{ConnectionId}` binding.
pub async fn call_at(
    engine: &Arc<InMemoryEngine>,
    host: &str,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> (u16, Bytes) {
    let (status, _headers, body) = call_with_headers(engine, host, method, path, body).await;
    (status, body)
}

/// As [`call_at`], and also the RESPONSE HEADERS.
///
/// A restJson1 client reads two of them off a failure before it looks at the body: the
/// `content-type` it decodes the envelope with, and the `x-amzn-errortype` it resolves
/// the error SHAPE from. They are part of the answer, so a suite that pins the envelope
/// needs them — see `connection_error_envelope`.
pub async fn call_with_headers(
    engine: &Arc<InMemoryEngine>,
    host: &str,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> (u16, http::HeaderMap, Bytes) {
    call_signed_by(engine, DEFAULT_ACCESS_KEY_ID, host, method, path, body).await
}

/// As [`call_with_headers`], presenting `access_key_id` as the request's SigV4 credential.
///
/// WHICH credential a call presents is part of the answer: every `@connections` call is
/// authorized against the principal that credential speaks for, so a suite about that
/// decision signs with the key of the session or user it seeded.
pub async fn call_signed_by(
    engine: &Arc<InMemoryEngine>,
    access_key_id: &str,
    host: &str,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> (u16, http::HeaderMap, Bytes) {
    let response = router(engine)
        .dispatch(request(access_key_id, host, method, path, body))
        .await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    (status, headers, response.into_body())
}

/// The error SHAPE a restJson1 failure names, read where a restJson1 client reads it: the
/// `x-amzn-errortype` response header.
///
/// NOT the body. A restJson1 error body carries the error shape's own modelled members and
/// nothing else — no shape discriminator — so the shape travels in this header alone (the
/// service's `rest_json_error_body` rule), and a memberless shape's body is `{}`.
pub fn error_code(headers: &http::HeaderMap) -> String {
    headers
        .get("x-amzn-errortype")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
        .unwrap_or_else(|| "<no x-amzn-errortype>".to_string())
}

/// The `message` member an error body carries, or `"<no message>"`.
pub fn error_message(body: &Bytes) -> String {
    let value: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// A response body rendered for an assertion message.
pub fn rendered(body: &Bytes) -> String {
    String::from_utf8_lossy(body).to_string()
}

/// The `ConnectionId` a recorded `@connections` caller addressed, and the all-`A` token with
/// the format marker in place: named here so a suite that needs one of them does not address
/// a catalog ROW BY POSITION, which reordering the catalog would silently change.
pub const RECORDED_ID: &str = "Fabc1cWIPHcCJ7A=";
pub const ALL_A_MARKED_ID: &str = "AAAAAAAAAAACAAA=";

/// Connection-id forms of the shape API Gateway ISSUES: the standard-alphabet base64,
/// canonically padded, of an 11-byte token carrying the format marker API Gateway's
/// connection-id encoding stamps (`mgmt::state::is_well_formed_connection_id`). Named once
/// here so every suite drives the same catalog; `connections_error_layer` asserts each one
/// IS well formed.
///
/// The first and last rows are tokens recorded `@connections` callers addressed; the rest
/// spell the character classes a token may use, each with the marker in place.
///
/// None of these carries `+` or `/`, so a suite can write one into a request path as it
/// stands; the percent-encoded form is driven by `connections_error_layer`.
pub const MINTED_IDS: [(&str, &str); 6] = [
    ("the padded base64 form AWS mints", "Lx9ZKcvPmjQCJhA="),
    ("an all-A token but for the marker", ALL_A_MARKED_ID),
    ("an all-z token but for the marker", "zzzzzzzzzzwCzz8="),
    ("a token of digits and letters", "0123456789cCQ4A="),
    ("an upper-case token", "ZZZZZZZZZZYCZZw="),
    ("the form the recorded calls use", RECORDED_ID),
];

/// How long an "overlong" malformed id is: far past the 16 characters a minted id has, so no
/// decoding of it can produce an 11-byte token.
const OVERLONG_ID_CHARS: usize = 200;

/// `ConnectionId` values that are NOT the form API Gateway issues, each with the way it
/// differs: too short, outside the base64 alphabet, unpadded, a base64url spelling, a token
/// of the wrong length, an overlong value, and tokens of the right length whose decoding
/// carries no connection-id format marker (the all-`A` token decodes to eleven zero bytes).
pub fn ids_api_gateway_does_not_mint() -> Vec<(String, String)> {
    [
        ("a single character", "x".to_string()),
        ("characters outside the base64 alphabet", "cid-with-dashes".to_string()),
        ("unpadded base64", "0123456789abcdef".to_string()),
        ("a base64url spelling", "a-b_c9DEF0123A4=".to_string()),
        ("base64 of a shorter token", "AAAAAAAAAAA=".to_string()),
        ("base64 of a longer token", "AAAAAAAAAAAAAAAAAAAA".to_string()),
        ("an overlong id", "A".repeat(OVERLONG_ID_CHARS)),
        ("the empty-looking padding alone", "====".to_string()),
        ("eleven zero bytes, so no format marker", "AAAAAAAAAAAAAAA=".to_string()),
        ("eleven bytes carrying another format byte", "zzzzzzzzzzzzzz8=".to_string()),
    ]
    .into_iter()
    .map(|(case, id)| (case.to_string(), id))
    .collect()
}

/// A stored instant rendered the way the modelled `__timestampIso8601`
/// (`@timestampFormat("date-time")`) members are serialized on the wire.
pub fn as_wire_timestamp(at: SystemTime) -> String {
    DateTime::from(at)
        .fmt(Format::DateTime)
        .expect("a stored instant renders as an ISO 8601 date-time")
}

/// Register a connection through the emulator-internal seam — the only way a
/// connection comes to exist. Panics if the seam declines, since every caller here
/// registers a fresh id.
pub fn register(
    engine: &Arc<InMemoryEngine>,
    connection_id: &str,
    source_ip: &str,
    user_agent: &str,
    connected_at: SystemTime,
) {
    register_identity(
        engine,
        connection_id,
        source_ip,
        Some(user_agent),
        connected_at,
    );
}

/// As [`register`], for a connection whose opening handshake may have named NO user agent:
/// `user_agent` is `None` for a client that sent no `User-Agent` field, which is a connection
/// with no user agent to report rather than one whose user agent is the empty string.
pub fn register_identity(
    engine: &Arc<InMemoryEngine>,
    connection_id: &str,
    source_ip: &str,
    user_agent: Option<&str>,
    connected_at: SystemTime,
) {
    assert!(
        mgmt::state::register_connection(
            &**engine,
            connection_id,
            mgmt::state::Identity {
                source_ip: source_ip.to_string(),
                user_agent: user_agent.map(str::to_string),
            },
            connected_at,
        ),
        "the registration seam must register {connection_id}"
    );
}

/// A `ConnectionId` of the shape API Gateway issues — standard base64 of
/// `mgmt::state::CONNECTION_ID_BYTES` bytes carrying the format marker its own encoding
/// stamps, which is the 16-character single-padded token its endpoints hand out — carrying
/// `label`, so each case addresses a connection of its own and nothing here spells a token
/// out by hand.
///
/// `label` is folded into exactly those bytes by the service's own encoder
/// (`mgmt::state::connection_id_for`): a shorter one is zero-filled, a longer one
/// truncated, and the marker is stamped either way, so every answer is a well-formed id
/// whatever the case called itself.
pub fn minted(label: &str) -> String {
    mgmt::state::connection_id_for(label.as_bytes())
}

/// The refusal a `ConnectionId` that API Gateway's connection-id encoding could never have
/// produced earns: the status, the shape a restJson1 client resolves from `x-amzn-errortype`,
/// and the FIXED text of its message, which the submitted value follows.
///
/// Named here, once, because every suite that drives one of the three operations asserts the
/// same two refusals and a second copy of either would let one suite accept what another
/// refuses.
pub const INVALID_STATUS: u16 = 400;
pub const INVALID_TYPE: &str = "BadRequestException";
pub const INVALID_MESSAGE: &str = "Invalid connectionId: ";

/// The modelled refusal for an id of that encoding which the registry does not hold
/// (`GoneException`, `@httpError(410)`, no members), and what [`error_message`] answers for
/// such a body, which carries no `message` member at all.
pub const GONE_STATUS: u16 = 410;
pub const GONE_TYPE: &str = "GoneException";
pub const NO_MESSAGE: &str = "<no message>";

/// The three operations this service models, by the method each is reached with and the
/// payload each sends — only `PostToConnection` carries a body.
pub fn operations() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("GET", Vec::new()),
        ("POST", b"fan-out".to_vec()),
        ("DELETE", Vec::new()),
    ]
}

/// A registration instant in the past, so an instant the service writes later is strictly
/// greater.
pub fn connected_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// The modelled request path for `connection_id`, spelled as a client sends it.
pub fn path(connection_id: &str) -> String {
    format!("/@connections/{}", as_path_segment(connection_id))
}

/// `connection_id` as a path SEGMENT carries it: the base64 alphabet's `+` and `/` and the
/// padding `=` are percent-encoded, which is how a client sends a token holding them.
///
/// One encoder for every suite, so no two of them can send the same token differently.
pub fn as_path_segment(connection_id: &str) -> String {
    connection_id
        .replace('%', "%25")
        .replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D")
}
