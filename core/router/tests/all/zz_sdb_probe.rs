// SPDX-License-Identifier: CC0-1.0

//! The probe `sdb_query_api_claims_its_own_requests.rs` drives the assembled emulator
//! with: how one Amazon SimpleDB request is addressed, written, dispatched and read back.
//!
//! It pins no rule of its own — the rule and its rows live in that suite — and sits in its
//! own file so that suite reads as the rule it pins rather than as request plumbing. The
//! form encoder and the XML element readers are this test crate's own, from
//! [`crate::query_wire`].

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is addressed in.
pub(crate) const REGION: &str = "us-east-1";

/// The emulator's own address, which is what a client is pointed at: an address, so the
/// `Host` header names no service.
pub(crate) const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The Amazon SimpleDB Query API version this endpoint serves, as its requests name it.
pub(crate) const API_VERSION: &str = "2009-04-15";

/// The XML namespace every document of that API carries.
pub(crate) const SDB_NAMESPACE: &str = "http://sdb.amazonaws.com/doc/2009-04-15/";

/// How a request names — or fails to name — the service it is for.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Addressed {
    /// NOTHING in the request names a service: no SigV4 credential scope, no
    /// `X-Amz-Target`, and the emulator's own loopback address in the `Host` header. This
    /// is how a conforming client of the 2009-04-15 API arrives, because that API is
    /// signed with Signature Version 2 — the credential travels among the request's own
    /// parameters, and no `Authorization` header is sent.
    Nothing,
    /// A SigV4 credential scope naming this endpoint's signing name.
    CredentialScope,
    /// This endpoint's own regional host, unsigned.
    EndpointHost,
}

impl Addressed {
    /// A one-character tag, so each addressing works on domains of its own.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Nothing => "u",
            Self::CredentialScope => "s",
            Self::EndpointHost => "h",
        }
    }

    fn host(self) -> String {
        match self {
            Self::Nothing | Self::CredentialScope => EMULATOR_HOST.to_string(),
            Self::EndpointHost => format!("sdb.{REGION}.amazonaws.com"),
        }
    }

    fn credential_scope(self) -> Option<String> {
        match self {
            Self::CredentialScope => Some(sdb_credential_scope()),
            Self::Nothing | Self::EndpointHost => None,
        }
    }
}

/// A credential scope naming this endpoint, the primary routing signal. The signature is
/// never verified here (see [`emulator`]); what the scope carries is the routing key.
fn sdb_credential_scope() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDSDBTEST/20260101/{REGION}/sdb/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one call answered.
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: String,
}

/// One AWS Query API call: a form-encoded POST of the root path, addressed as `addressed`
/// says. The body is written with this test crate's own form encoder
/// ([`crate::query_wire::form_encode`]), so a value carrying a reserved character reaches
/// the service exactly as an SDK would send it.
pub(crate) fn query(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    addressed: Addressed,
    parameters: &[(&str, &str)],
) -> Answer {
    let form = parameters
        .iter()
        .map(|(name, value)| format!("{}={}", form_encode(name), form_encode(value)))
        .collect::<Vec<String>>()
        .join("&");
    let host = addressed.host();
    let mut builder = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host.as_str())
        .header(
            "content-type",
            "application/x-www-form-urlencoded; charset=utf-8",
        );
    if let Some(scope) = addressed.credential_scope() {
        builder = builder.header("authorization", scope);
    }
    let request = builder
        .body(bytes::Bytes::from(form))
        .expect("a well-formed test request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// One call of the 2009-04-15 API that must be SERVED, returning the document it answered
/// with: HTTP 200 and that action's own response envelope in that API's namespace.
pub(crate) fn served(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    addressed: Addressed,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut form: Vec<(&str, &str)> = vec![("Action", action), ("Version", API_VERSION)];
    form.extend_from_slice(parameters);
    let answer = query(router, runtime, addressed, &form);
    assert_eq!(
        answer.status, 200,
        "case {action} addressed {addressed:?}: expected HTTP 200, got {answer:?}"
    );
    assert!(
        answer
            .body
            .contains(&format!("<{action}Response xmlns=\"{SDB_NAMESPACE}\">")),
        "case {action} addressed {addressed:?}: expected the {action}Response envelope in \
         the 2009-04-15 namespace, got {answer:?}"
    );
    answer.body
}

/// One modelled `/v2/*` export call, addressed by this endpoint's credential scope, which
/// is how the modelled surface's own SigV4 client addresses it.
pub(crate) fn modelled(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/v2/{operation}"))
        .header("host", EMULATOR_HOST)
        .header("content-type", "application/json")
        .header("authorization", sdb_credential_scope())
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, body) = response.into_parts();
    let answered: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (parts.status.as_u16(), answered)
}

/// The assembled emulator — every service on one engine — and the runtime its requests are
/// driven on.
///
/// Request authentication is off: these cases are about which service CLAIMS and SERVES a
/// request, and a credential scope a case carries is a routing signal rather than a
/// signature anything verifies.
pub(crate) fn emulator() -> (MultiServiceRouter, tokio::runtime::Runtime) {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime");
    (router, runtime)
}

/// The assembled emulator with request authentication ON, as the emulator serves traffic
/// by default, and the runtime its requests are driven on.
pub(crate) fn emulator_verifying() -> (MultiServiceRouter, tokio::runtime::Runtime) {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(true);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime");
    (router, runtime)
}

/// Run `cases` on a thread with the stack assembling the whole router needs.
pub(crate) fn on_a_sized_thread(name: &'static str, cases: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(cases)
        .expect("the sized thread spawns")
        .join()
        .expect("the suite runs to completion");
}
