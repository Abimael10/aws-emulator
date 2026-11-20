// SPDX-License-Identifier: CC0-1.0

//! The harness every suite in this crate drives the service through.
//!
//! The suites of `tests/all.rs` compile into ONE test crate, so each of the rules
//! below lives here exactly once rather than once per suite — they are rules, not
//! repeated literals: how a request reaches this service, which bytes of a path
//! segment or a query value an SDK escapes, and how the modelled error code is read
//! off a failure response whichever place the protocol put it.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pcaconnectorad_pcaconnectorad as pcad;

/// The Region every suite's SigV4 credential scope names, so every minted ARN must
/// carry it.
pub const REGION: &str = "us-west-2";

/// The canonical emulator account id every ARN carries.
pub const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// A router serving this service alone, over the shared engine the production
/// router builds — and that engine, for a case that needs the instant a
/// clock-owed transition is due to be in a known position instead of racing it.
pub fn router_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised
    // elsewhere; these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(pcad::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// [`router_with_engine`] for a case that drives everything over the wire.
pub fn router() -> MultiServiceRouter {
    router_with_engine().0
}

/// Register `directory_id` with this service and answer the registration's ARN —
/// the prerequisite `CreateConnector` resolves its `DirectoryId` against, so a
/// suite that wants a connector asks for this first.
///
/// Idempotent: the `ClientToken` is derived from the directory id, so registering
/// the same directory twice answers the first registration's ARN instead of the
/// conflict a second registration of one directory is.
pub async fn register_directory(router: &MultiServiceRouter, directory_id: &str) -> String {
    let body = serde_json::json!({
        "DirectoryId": directory_id,
        "ClientToken": format!("register-{directory_id}"),
    });
    let (status, code, parsed) = call(router, "POST", "/directoryRegistrations", Some(&body)).await;
    assert_eq!(
        status, 202,
        "setup {directory_id}: CreateDirectoryRegistration must answer the modelled 202, \
         got {status} / {code:?} / {parsed}"
    );
    parsed["DirectoryRegistrationArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup {directory_id}: the create answered no DirectoryRegistrationArn: {parsed}")
        })
        .to_string()
}

/// Percent-encode one value the way an AWS SDK encodes a non-greedy `@httpLabel`
/// or a query-string value: every byte outside the unreserved set is escaped, so
/// an ARN's `:` and `/` arrive as `%3A` / `%2F` and stay inside one segment, and a
/// token's `=` padding cannot read as a parameter separator.
pub fn encode_label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The access key id every call signs with unless a suite names another: a key neither STS
/// nor IAM holds a record for, which is the position AWS account root credentials are in.
pub const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// One request through the same `MultiServiceRouter::dispatch` the production front
/// door uses: the HTTP status, the modelled error code (empty on success) and the
/// parsed body (`Value::Null` when there was none).
pub async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path_and_query: &str,
    body: Option<&Value>,
) -> (u16, String, Value) {
    call_as(router, ACCOUNT_KEY_ID, method, path_and_query, body).await
}

/// The host every call addresses unless a suite names another: the endpoint host AWS
/// publishes this service at in [`REGION`], which is what an AWS SDK sends.
pub fn aws_endpoint_host() -> String {
    format!("pca-connector-ad.{REGION}.amazonaws.com")
}

/// [`call`] signed with `access_key_id`, which is what names the principal the request
/// speaks for — the one thing a suite about WHO may call this service has to vary.
pub async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path_and_query: &str,
    body: Option<&Value>,
) -> (u16, String, Value) {
    call_at_host(
        router,
        &aws_endpoint_host(),
        access_key_id,
        method,
        path_and_query,
        body,
    )
    .await
}

/// [`call_as`] addressed to `host`, which is what a suite about the address a request
/// REACHED this emulator on varies: a client configured with one local endpoint URL sends
/// that endpoint's own host, an AWS SDK sends the service's published endpoint host. The
/// request still carries its SigV4 credential scope, which is the signal that routes it,
/// so the host chooses nothing but the address the request arrived on.
pub async fn call_at_host(
    router: &MultiServiceRouter,
    host: &str,
    access_key_id: &str,
    method: &str,
    path_and_query: &str,
    body: Option<&Value>,
) -> (u16, String, Value) {
    let payload = match body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path_and_query}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/pca-connector-ad/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(payload)
        .expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let code = error_code(&response).unwrap_or_default();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, code, parsed)
}

/// The modelled error shape name of a failure response, from the
/// `x-amzn-errortype` header or the body's `__type`, with any namespace prefix and
/// URL suffix stripped. `None` for a success.
fn error_code(response: &http::Response<bytes::Bytes>) -> Option<String> {
    if response.status().is_success() {
        return None;
    }
    let from_header = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let from_body = || {
        let parsed: Value = serde_json::from_slice(response.body()).ok()?;
        parsed
            .get("__type")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let raw = from_header.or_else(from_body)?;
    let no_url = raw.split(':').next().unwrap_or(&raw).to_string();
    Some(no_url.split('#').next_back().unwrap_or(&no_url).to_string())
}
