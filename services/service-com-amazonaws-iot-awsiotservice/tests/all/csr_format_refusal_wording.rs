// SPDX-License-Identifier: CC0-1.0

//! The sentence `CreateCertificateFromCsr` refuses material it cannot read as a
//! certification request with, over the wire.
//!
//! The rule under test: when the `certificateSigningRequest` a caller submits cannot be
//! read as a PEM-encoded PKCS#10 certification request, AWS IoT answers HTTP 400
//! `InvalidRequestException` with ONE fixed sentence — "Invalid CSR format" — which has no
//! variable part: neither the member that carried the material, nor the bytes it held, nor
//! what about them could not be read appears in it. So every shape of unreadable material
//! earns the SAME message, whatever it was.
//!
//! The table below spans what that rule generalises over — the FORM of the material (no PEM
//! armor at all, armor of another label, armor whose body is not base64, armor whose body
//! is not a `CertificationRequest`, a truncated block, bare base64 DER), its SIZE (one
//! character, the modelled `@length` maximum of 4096), its TYPE (text, JSON, a readable
//! X.509 certificate rather than a request) — and a readable request on a documented key
//! is driven too, so what the rows pin is this refusal and not a handler that refuses
//! everything.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the suite signs for.
const REGION: &str = "us-east-1";

/// The longest `certificateSigningRequest` the model admits: `CertificateSigningRequest`
/// carries `@length min 1 max 4096`
/// (`smithy_query(query="operation",operation="CreateCertificateFromCsr")`).
const MAX_CSR_LENGTH: usize = 4096;

/// The one sentence AWS IoT states this refusal in.
const INVALID_CSR_FORMAT: &str = "Invalid CSR format";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service. The signature itself is never
/// checked — the harness turns request authentication off, as an unauthenticated emulator
/// runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The error shape the answer names, as a client reads it: the `x-amzn-errortype`
    /// header up to the `:` that introduces the shape's documentation URI.
    fn code(&self) -> Option<&str> {
        self.error_code
            .as_deref()
            .map(|value| value.split(':').next().unwrap_or(value))
    }

    /// The `message` of an error body.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }
}

/// The emulator under test: one engine with the IoT service mounted on it, reached the way
/// the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// `CreateCertificateFromCsr`: `POST /certificates`, the request in the body.
    async fn create(&self, csr: &str) -> Reply {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!(
                "https://iot.{REGION}.amazonaws.com/certificates?setAsActive=true"
            ))
            .header("authorization", signed_for_iot(REGION))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(
                json!({"certificateSigningRequest": csr})
                    .to_string()
                    .into_bytes(),
            ))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }
}

/// A readable PKCS#10 certification request over a key the operation documents — an ECDSA
/// P-256 key, which is `rcgen`'s default and one of "NIST P-256, NIST P-384, or NIST P-521"
/// (`smithy_query(query="doc",shape="CreateCertificateFromCsr")`).
fn readable_certification_request(common_name: &str) -> String {
    let key = ::rcgen::KeyPair::generate().expect("a key pair is generated");
    let mut params =
        ::rcgen::CertificateParams::new(Vec::<String>::new()).expect("certificate parameters");
    let mut subject = ::rcgen::DistinguishedName::new();
    subject.push(::rcgen::DnType::CommonName, common_name);
    params.distinguished_name = subject;
    params
        .serialize_request(&key)
        .expect("a certification request")
        .pem()
        .expect("the certification request encodes as PEM")
}

/// A readable self-signed X.509 CERTIFICATE: material of the right PEM form whose structure
/// is not a certification request.
fn readable_certificate(common_name: &str) -> String {
    let key = ::rcgen::KeyPair::generate().expect("a key pair is generated");
    let mut params =
        ::rcgen::CertificateParams::new(Vec::<String>::new()).expect("certificate parameters");
    let mut subject = ::rcgen::DistinguishedName::new();
    subject.push(::rcgen::DnType::CommonName, common_name);
    params.distinguished_name = subject;
    params
        .self_signed(&key)
        .expect("a self-signed certificate")
        .pem()
}

/// The material a row submits, labelled by what makes it unreadable.
fn unreadable_material() -> Vec<(String, String)> {
    let request = readable_certification_request("sensor-00");
    let body: String = request
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let header_only = request
        .lines()
        .next()
        .expect("the request has a header line")
        .to_string();

    vec![
        ("plain text, no PEM armor".to_string(), "not-a-csr".to_string()),
        (
            "one character, the modelled length minimum".to_string(),
            "x".to_string(),
        ),
        ("whitespace only".to_string(), "  \n\t \r\n ".to_string()),
        (
            "the modelled length maximum of filler".to_string(),
            "A".repeat(MAX_CSR_LENGTH),
        ),
        (
            "a JSON document".to_string(),
            json!({"certificateSigningRequest": "nested"}).to_string(),
        ),
        (
            "bare base64 DER with the armor stripped".to_string(),
            body,
        ),
        ("a truncated PEM block".to_string(), header_only),
        (
            "PEM armor of another label".to_string(),
            "-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n".to_string(),
        ),
        (
            "request armor over a body that is not base64".to_string(),
            "-----BEGIN CERTIFICATE REQUEST-----\n!!!! not base64 !!!!\n\
             -----END CERTIFICATE REQUEST-----\n"
                .to_string(),
        ),
        (
            "request armor over a body that is not a CertificationRequest".to_string(),
            "-----BEGIN CERTIFICATE REQUEST-----\nZGVmaW5pdGVseSBub3QgREVS\n\
             -----END CERTIFICATE REQUEST-----\n"
                .to_string(),
        ),
        (
            "a readable certificate rather than a request".to_string(),
            readable_certificate("sensor-01"),
        ),
    ]
}

#[tokio::test]
async fn every_unreadable_csr_is_refused_with_one_fixed_sentence() {
    let emulator = Emulator::new();

    for (label, material) in unreadable_material() {
        let refused = emulator.create(&material).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: expected status 400, got {refused:?}"
        );
        assert_eq!(
            refused.code(),
            Some("InvalidRequestException"),
            "case {label:?}: expected error code InvalidRequestException, got {refused:?}"
        );
        let message = refused.message();
        assert_eq!(
            message, INVALID_CSR_FORMAT,
            "case {label:?}: expected message {INVALID_CSR_FORMAT:?}, got {message:?}"
        );
        // The sentence has no variable part, so nothing of the request reaches it: not the
        // member that carried the material, and not the material itself.
        assert!(
            !message.contains("certificateSigningRequest"),
            "case {label:?}: the message must not name the input member, got {message:?}"
        );
        for fragment in material.split_whitespace().filter(|part| part.len() >= 4) {
            assert!(
                !message.contains(fragment),
                "case {label:?}: the message must not echo the submitted material \
                 ({fragment:?}), got {message:?}"
            );
        }
        assert!(
            refused.text("certificateId").is_none(),
            "case {label:?}: a refusal reports no certificate, got {refused:?}"
        );
    }
}

#[tokio::test]
async fn a_readable_request_on_a_documented_key_is_not_refused() {
    let emulator = Emulator::new();

    // The contrast row: the refusal above is earned by material that is not a
    // certification request, not by every request this operation is handed.
    let issued = emulator
        .create(&readable_certification_request("sensor-02"))
        .await;
    assert_eq!(
        issued.status, 200,
        "a readable request on an ECDSA P-256 key must be issued a certificate, got {issued:?}"
    );
    assert!(
        issued.text("certificateId").is_some(),
        "the issued certificate must be reported, got {issued:?}"
    );
}
