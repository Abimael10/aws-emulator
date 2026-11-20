// SPDX-License-Identifier: CC0-1.0

//! The shared wire harness every suite in this crate drives requests through.
//!
//! Each request goes through the real restJson1 front door —
//! [`MultiServiceRouter::dispatch`] — so routing, URI/query binding, constraint
//! validation, the handler, and response serialization are all exercised. The
//! state lives in one shared engine for the whole router, so a value written by
//! one request is read back by the next.

use std::collections::BTreeMap;
use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

/// The region the requests below are signed for, unless a case says otherwise.
/// It is deliberately NOT the emulator's fallback region, so an ARN that ignored
/// the request's credential scope would be visible.
pub const REGION: &str = "eu-west-2";

/// The account every emulated ARN carries.
pub fn account_id() -> &'static str {
    engine::emulator_account_id()
}

/// A router with just this service mounted on one shared engine.
pub fn scheduler_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These tests drive the wire, not SigV4: the Authorization header below is
    // only how a request names the service and its region.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(engine),
    );
    router
}

/// A request addressed to `scheduler` in `region`, the way a signed SDK call is.
pub fn request(
    method: &str,
    uri: &str,
    region: &str,
    body: Option<serde_json::Value>,
) -> http::Request<bytes::Bytes> {
    let credential = format!("AKIDTESTTESTTEST/20260101/{region}/scheduler/aws4_request");
    let body = body.map_or_else(bytes::Bytes::new, |json| {
        bytes::Bytes::from(json.to_string())
    });
    http::Request::builder()
        .method(method)
        .uri(uri)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={credential}, SignedHeaders=host, Signature=abc123"
            ),
        )
        .header("content-type", "application/json")
        .body(body)
        .expect("a well-formed test request")
}

/// Dispatch and return `(status, parsed body)`. An empty body parses as `null`.
pub async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    call_in(router, method, uri, REGION, body).await
}

/// Dispatch a request signed for `region`, and return `(status, parsed body)`.
pub async fn call_in(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    region: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let response = router.dispatch(request(method, uri, region, body)).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, parsed)
}

/// The wire error code restJson1 puts in `__type`, without its namespace.
pub fn error_code(body: &serde_json::Value) -> String {
    body.get("__type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// The `Message` a restJson1 error carries, or an empty string when it carries
/// none. The envelope spells the member `Message`; `message` is accepted too, so
/// a suite asserts the TEXT rather than the casing.
pub fn error_message(body: &serde_json::Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Percent-encode one URI path segment or query value, the way an SDK does: an
/// ARN carries `:` and `/`, and `/tags/{ResourceArn}` is a single (non-greedy)
/// label, so everything but the unreserved characters has to be encoded for the
/// value to survive the server's percent-decode unchanged.
pub fn encode_label(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(*byte));
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// The text of the first `<tag>` element of an `awsQuery` answer, or `None` for an answer
/// carrying no such element.
///
/// The ONE reading of a tag's text this test crate keeps: an `awsQuery` answer is XML, and
/// three suites read a single element out of one (the role a pass was judged on, the action a
/// refusal named, the status a DB instance reports), so they share this rather than each
/// hand-rolling the same two `find`s.
pub fn xml_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// The test clock as seconds since the epoch — the unit restJson1 renders a
/// `timestamp` member in, so a suite can bracket a server-set date with its own
/// readings taken around the call that set it.
pub fn now_epoch_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the test clock is after the epoch")
        .as_secs_f64()
}

/// The ARN `CreateScheduleGroup` mints for `name` in `region`.
pub fn group_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:scheduler:{region}:{}:schedule-group/{name}",
        account_id()
    )
}

/// `GET /tags/{ResourceArn}`: `ResourceArn` is a non-greedy `@httpLabel`, so it
/// travels as ONE percent-encoded path segment.
pub async fn list_tags(router: &MultiServiceRouter, arn: &str) -> (u16, serde_json::Value) {
    call(router, "GET", &format!("/tags/{}", encode_label(arn)), None).await
}

/// `ListTagsForResource`'s answer as the map it represents, so a comparison does
/// not depend on an order AWS never promises.
pub fn tags_of(body: &serde_json::Value) -> BTreeMap<String, String> {
    body.get("Tags")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("no Tags list in {body}"))
        .iter()
        .map(|tag| {
            (
                tag.get("Key")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tag.get("Value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}
