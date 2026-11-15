// SPDX-License-Identifier: CC0-1.0

//! The harness every suite in this crate drives the service through.
//!
//! The suites of `tests/all.rs` compile into ONE test crate, so each of the rules below can
//! live here exactly once. They are rules, not repeated literals — which bytes of a path
//! segment an SDK escapes, how a request reaches this service, how to move the instant the
//! clock owes a connector its pending transition — so a private copy of one in a suite is a
//! copy that can drift away from the others:
//!
//! * [`new_router`], [`call`] / [`call_with_scope`] and [`Answer`]: one HTTP request per call
//!   through the same `MultiServiceRouter` the emulator boots, carrying the SigV4 credential
//!   scope a handler reads the caller's Region out of, and what came back.
//! * [`label`]: the `@httpLabel` percent-encoding rule.
//! * [`assert_tagging_not_found`], [`assert_tagging_bad_request`] and
//!   [`assert_tagging_limit_exceeded`]: the refusals the tagging endpoints answer with, which
//!   are the tagging layer's own error classes and not this service's.
//! * [`active_private_ca`], [`pending_certificate_private_ca`] and
//!   [`release_private_ca_clock`]: the private certificate authority a connector links, built
//!   through Amazon Web Services Private CA's OWN front door and driven to the status a case
//!   needs, because `CreateConnector` resolves that reference against that service.
//! * [`set_pending_due`] and the four helpers over it: the clock move, so a case about a
//!   transient status neither sleeps for the transition span nor races it.
//!
//! The clock move is ONE rule with two intents a case can want. [`set_pending_due`] states
//! the rule and answers whether anything was owed; [`release_pending`] and [`hold_pending`]
//! require a transition to be owed, and the `_if_owed` pair tolerates there being none and
//! reports it, for a case whose record may already have settled or been torn down.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pcaconnectorscep_pcaconnectorscep as scep;

/// How far in the past a released transition's due instant is put: any past instant is due,
/// so the next call settles it.
const ALREADY_DUE: Duration = Duration::from_secs(1);

/// How far in the future a held transition's due instant is put: longer than any case takes,
/// so every call in it observes the transient status.
const NOT_DUE_FOR: Duration = Duration::from_secs(3600);

/// What one call answered: the HTTP status, the modelled error code (empty on success), the
/// raw body bytes and the parsed body (`Value::Null` when there were none).
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub error_type: String,
    pub raw: Vec<u8>,
    pub body: Value,
}

impl Answer {
    /// One member of the response body, as a string.
    pub fn text(&self, name: &str) -> String {
        self.body[name]
            .as_str()
            .unwrap_or_else(|| panic!("expected a string `{name}` in {:?}", self.body))
            .to_string()
    }

    /// The returned `Tags` map, which an implemented listing always carries.
    pub fn tags(&self) -> &Map<String, Value> {
        self.body["Tags"]
            .as_object()
            .unwrap_or_else(|| panic!("expected a `Tags` map in {:?}", self.body))
    }

    /// How many entries a listing's item member carries (absent counts as none).
    pub fn count(&self, member: &str) -> usize {
        self.body[member].as_array().map_or(0, Vec::len)
    }

    /// The `Message` an error body carries, or the empty string when it carries none.
    pub fn message(&self) -> &str {
        self.body["Message"].as_str().unwrap_or_default()
    }

    /// One member of the returned `Connector`, or `Value::Null` when it carries none.
    pub fn connector(&self, name: &str) -> &Value {
        &self.body["Connector"][name]
    }

    /// One member of the returned `Challenge`, or `Value::Null` when it carries none.
    pub fn challenge(&self, name: &str) -> &Value {
        &self.body["Challenge"][name]
    }
}

/// The router, with the shared engine it was built from, so a case that needs a clock-driven
/// transition to be in a known position can put it there instead of racing it.
pub fn new_router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere;
    // these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(scep::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Percent-encode a value for use in a single `@httpLabel` path segment or a query-string
/// value, the way an AWS SDK does: every byte outside the unreserved set is escaped, so the
/// `:` and `/` inside an ARN cannot read as path structure and the `=` padding of a
/// `NextToken` cannot read as a parameter separator. One rule covers both places.
pub fn label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// One request, addressed at `region`'s endpoint and signed for the same Region.
pub async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    call_with_scope(router, region, region, method, path, body).await
}

/// One request addressed at `host_region`'s endpoint but carrying `scope_region` in its SigV4
/// credential scope — which is how a handler learns the Region the caller is addressing — so a
/// case can send a scope Region no endpoint of this service has.
pub async fn call_with_scope(
    router: &MultiServiceRouter,
    host_region: &str,
    scope_region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = match body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://pca-connector-scep.{host_region}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{scope_region}/pca-connector-scep/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let raw = response.into_body().to_vec();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        raw,
        body,
    }
}

/// Assert `answer` is the refusal the resource-tagging endpoints (`/tags/{ResourceArn}`, by
/// method) answer an ARN that names no resource with.
///
/// Those three operations are served by the resource-tagging layer, which reports an ARN it
/// resolves to nothing with its OWN error class rather than with this service's
/// `ResourceNotFoundException`: HTTP 400, the `NotFoundException` error code, and a single
/// lower-case `message` member carrying one fixed sentence. The literals are the recorded
/// wire values of that refusal, spelled here rather than read back out of the
/// implementation.
pub fn assert_tagging_not_found(answer: &Answer, case: &str) {
    assert_tagging_refusal(answer, "NotFoundException", "Resource was not found", case);
}

/// Assert `answer` is the refusal those same endpoints answer a `ResourceArn` the tagging
/// layer cannot parse as an ARN with.
///
/// The layer parses the value before it resolves anything, so a value that is not an ARN is
/// refused by the layer itself rather than reported against the member: HTTP 400, the
/// `BadRequestException` error code, and one fixed lower-case `message`. The literals are the
/// recorded wire values of that refusal.
pub fn assert_tagging_bad_request(answer: &Answer, case: &str) {
    assert_tagging_refusal(
        answer,
        "BadRequestException",
        "Invalid input resource arn: bad syntax",
        case,
    );
}

/// Assert `answer` is the refusal those same endpoints answer a write whose RESULTANT tag set
/// would be over the per-resource ceiling with.
///
/// The ceiling belongs to the tagging layer and not to this service's input model, so it is
/// that layer's own `BadRequestException` class with its own fixed sentence — the literal is
/// the recorded wire value of that refusal, spelled here rather than read back out of the
/// implementation.
pub fn assert_tagging_limit_exceeded(answer: &Answer, case: &str) {
    assert_tagging_refusal(
        answer,
        "BadRequestException",
        "The resultant tag set must not have more than 50 user tags.",
        case,
    );
}

/// The shape both of the tagging layer's refusals share: the status, its own error code, one
/// fixed sentence under the lower-case `message` member — no value of the request, and none of
/// the service error's `Message` / `ResourceId` / `ResourceType` members.
fn assert_tagging_refusal(answer: &Answer, code: &str, message: &str, case: &str) {
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (400, code),
        "case {case:?}: expected the tagging layer's 400 {code}, got {} / {:?}",
        answer.status,
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case:?}: expected the fixed lower-case `message` of the tagging layer's \
         refusal, got {:?}",
        answer.body
    );
    for member in ["Message", "ResourceId", "ResourceType"] {
        assert!(
            answer.body.get(member).is_none(),
            "case {case:?}: the tagging layer's refusal carries no {member:?} member, got {:?}",
            answer.body
        );
    }
}

/// The template that produces a ROOT CA's own certificate — step 2 of Amazon Web Services
/// Private CA's documented procedure for creating a root private CA, which is how a case
/// drives a CA to `ACTIVE`.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// A Private CA client for `region` over the router's own in-process callback — the very seam
/// this service resolves a `CertificateAuthorityArn` through. No socket is opened; the
/// endpoint URL is only the address that transport routes by.
pub fn private_ca_client(router: &MultiServiceRouter, region: &str) -> ::aws_sdk_acmpca::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_acmpca::Config::builder()
        .behavior_version(::aws_sdk_acmpca::config::BehaviorVersion::latest())
        .region(::aws_sdk_acmpca::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://acm-pca.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_acmpca::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_acmpca::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_acmpca::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_acmpca::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_acmpca::Client::from_conf(config)
}

/// Make every transition Amazon Web Services Private CA owes come due at once, through that
/// service's own state.
///
/// Strictly a clock move — it rewrites only the due instants (a CA's settle, an issued
/// certificate's availability), never a status or a stored value — so that service's own
/// handlers still compute what they report from their own records. A case neither sleeps for
/// one of those spans nor races it.
pub fn release_private_ca_clock(engine: &engine::InMemoryEngine) {
    let shared = service_com_amazonaws_acmpca_acmprivateca::state::service_state(engine)
        .expect("the private CA service's own state");
    let mut store = shared.lock();
    let already_due = SystemTime::now() - ALREADY_DUE;
    for authority in store.certificate_authorities.values_mut() {
        authority.settles_at = already_due;
        for certificate in authority.certificates.values_mut() {
            certificate.available_at = already_due;
        }
    }
}

/// A private CA of `region`, exactly as Private CA's own `CreateCertificateAuthority` leaves
/// it: the transition that service owes it is NOT made due, so it is still reported in the
/// status a create starts it in.
pub async fn created_private_ca(
    router: &MultiServiceRouter,
    region: &str,
    common_name: &str,
) -> String {
    let subject = ::aws_sdk_acmpca::types::Asn1Subject::builder()
        .common_name(common_name)
        .build();
    let configuration = ::aws_sdk_acmpca::types::CertificateAuthorityConfiguration::builder()
        .key_algorithm(::aws_sdk_acmpca::types::KeyAlgorithm::Rsa2048)
        .signing_algorithm(::aws_sdk_acmpca::types::SigningAlgorithm::Sha256Withrsa)
        .subject(subject)
        .build()
        .expect("the CA configuration builds");
    private_ca_client(router, region)
        .create_certificate_authority()
        .certificate_authority_configuration(configuration)
        .certificate_authority_type(::aws_sdk_acmpca::types::CertificateAuthorityType::Root)
        .send()
        .await
        .expect("Private CA must create the certificate authority")
        .certificate_authority_arn()
        .expect("Private CA must answer the CA's ARN")
        .to_string()
}

/// A private CA of `region`, created through Private CA's own `CreateCertificateAuthority`
/// and left where that call leaves it: existing, and waiting for the certificate that would
/// put it in service (`PENDING_CERTIFICATE`).
pub async fn pending_certificate_private_ca(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    region: &str,
    common_name: &str,
) -> String {
    let arn = created_private_ca(router, region, common_name).await;
    // The CREATING -> PENDING_CERTIFICATE move that service owes the CA.
    release_private_ca_clock(engine);
    arn
}

/// A private CA of `region` driven all the way to `ACTIVE` through Private CA's own documented
/// procedure, answering its ARN and the PEM certificate that service then HOLDS for it.
///
/// `CreateConnector` admits only a CA that service reports `ACTIVE`, so this is the fixture a
/// case that needs a connector at all starts from.
pub async fn active_private_ca(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    region: &str,
    common_name: &str,
) -> (String, String) {
    let arn = pending_certificate_private_ca(router, engine, region, common_name).await;
    let held = activate_private_ca(router, engine, region, &arn).await;
    (arn, held)
}

/// Put the EXISTING private CA `arn` in service: its CSR is signed with the root template and
/// the certificate is imported back, which is Private CA's own documented procedure for a root
/// CA. Answers the PEM certificate that service then holds for it.
///
/// Separate from [`active_private_ca`] because the precondition a create applies is about the
/// status at the instant of the call: a case shows that by making the same call before and
/// after this.
pub async fn activate_private_ca(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    region: &str,
    arn: &str,
) -> String {
    let client = private_ca_client(router, region);
    let csr = client
        .get_certificate_authority_csr()
        .certificate_authority_arn(arn)
        .send()
        .await
        .expect("Private CA must answer the CA's CSR")
        .csr()
        .expect("the CSR is present")
        .to_string();
    let certificate_arn = client
        .issue_certificate()
        .certificate_authority_arn(arn)
        .csr(::aws_smithy_types::Blob::new(csr.into_bytes()))
        .signing_algorithm(::aws_sdk_acmpca::types::SigningAlgorithm::Sha256Withrsa)
        .template_arn(ROOT_CA_TEMPLATE)
        .validity(
            ::aws_sdk_acmpca::types::Validity::builder()
                .r#type(::aws_sdk_acmpca::types::ValidityPeriodType::Years)
                .value(10)
                .build()
                .expect("the validity builds"),
        )
        .send()
        .await
        .expect("Private CA must issue the CA's own certificate")
        .certificate_arn()
        .expect("Private CA must answer the certificate's ARN")
        .to_string();
    // The instant that certificate becomes readable is one that service's clock owes.
    release_private_ca_clock(engine);

    let certificate = client
        .get_certificate()
        .certificate_authority_arn(arn)
        .certificate_arn(&certificate_arn)
        .send()
        .await
        .expect("Private CA must answer the issued certificate")
        .certificate()
        .expect("the certificate is present")
        .to_string();
    client
        .import_certificate_authority_certificate()
        .certificate_authority_arn(arn)
        .certificate(::aws_smithy_types::Blob::new(certificate.into_bytes()))
        .send()
        .await
        .expect("Private CA must import the CA's certificate");

    // The certificate the CA now HOLDS, read through its own front door rather than assumed
    // to be the one that was imported.
    client
        .get_certificate_authority_certificate()
        .certificate_authority_arn(arn)
        .send()
        .await
        .expect("Private CA must answer the CA's own certificate")
        .certificate()
        .expect("the certificate is present")
        .to_string()
}

/// The status Private CA reports for `arn` — ASKED, never assumed, so a case's expectation
/// rests on that service's own report of the CA it built.
pub async fn reported_private_ca_status(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
) -> String {
    private_ca_client(router, region)
        .describe_certificate_authority()
        .certificate_authority_arn(arn)
        .send()
        .await
        .expect("Private CA must describe the certificate authority")
        .certificate_authority()
        .and_then(|authority| authority.status())
        .expect("Private CA must report the CA's status")
        .as_str()
        .to_string()
}

/// Take the private CA `arn` away through Private CA's own front door, all the way to
/// PERMANENTLY deleted: disabled, deleted, and then its restoration window closed by a clock
/// move — after which that service holds nothing under the ARN and answers no certificate for
/// it.
///
/// For a case about a connector whose CA is no longer there: `CreateConnector` admits only a
/// CA that service reports `ACTIVE`, so a connector linking an unreadable CA is one whose CA
/// went away after it was created.
pub async fn permanently_delete_private_ca(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    region: &str,
    arn: &str,
) {
    let client = private_ca_client(router, region);
    // "To delete a CA in the ACTIVE state, you must first disable it"
    // (`DeleteCertificateAuthority` docs).
    client
        .update_certificate_authority()
        .certificate_authority_arn(arn)
        .status(::aws_sdk_acmpca::types::CertificateAuthorityStatus::Disabled)
        .send()
        .await
        .expect("Private CA must disable the certificate authority");
    client
        .delete_certificate_authority()
        .certificate_authority_arn(arn)
        .permanent_deletion_time_in_days(
            service_com_amazonaws_acmpca_acmprivateca::state::MIN_PERMANENT_DELETION_TIME_IN_DAYS,
        )
        .send()
        .await
        .expect("Private CA must delete the certificate authority");

    // The end of the restoration period is an instant that service's clock owes: moving it
    // into the past is a clock move, and that service's own handlers are what then report the
    // CA as gone.
    let shared = service_com_amazonaws_acmpca_acmprivateca::state::service_state(engine)
        .expect("the private CA service's own state");
    let mut store = shared.lock();
    let already_due = SystemTime::now() - ALREADY_DUE;
    for authority in store.certificate_authorities.values_mut() {
        if authority.restorable_until.is_some() {
            authority.restorable_until = Some(already_due);
        }
    }
}

/// Move the instant the clock owes `arn` its pending transition to `due_at`, through the
/// service's own state.
///
/// Strictly a clock move: it rewrites only the pending transition's due instant, never the
/// status, the timestamps or the outcome — the handlers still compute those from this same
/// record by settling it. Answers whether a transition was there to move: `false` once the
/// record has settled, its teardown has completed, or nothing was ever created under `arn`.
pub fn set_pending_due(engine: &engine::InMemoryEngine, arn: &str, due_at: SystemTime) -> bool {
    let shared = scep::state::service_state(engine).expect("the service's own state");
    let mut store = shared.lock();
    match store
        .connectors
        .get_mut(arn)
        .and_then(|connector| connector.pending.as_mut())
    {
        Some(pending) => {
            pending.due_at = due_at;
            true
        }
        None => false,
    }
}

/// Make the transition the clock owes `arn` due, so the next call materialises it: `CREATING`
/// into `ACTIVE`, or a `DELETING` connector's teardown into the record being gone.
///
/// Requires a transition to be owed; use [`release_pending_if_owed`] where a case tolerates
/// there being none.
pub fn release_pending(engine: &engine::InMemoryEngine, arn: &str) {
    assert!(
        release_pending_if_owed(engine, arn),
        "expected a transition the clock owes {arn:?}: either no connector is stored under \
         that ARN or it has already settled"
    );
}

/// Keep the transition the clock owes `arn` undue for the rest of the case, so every call in
/// it observes the transient status.
///
/// Requires a transition to be owed; use [`hold_pending_if_owed`] where a case tolerates
/// there being none.
pub fn hold_pending(engine: &engine::InMemoryEngine, arn: &str) {
    assert!(
        hold_pending_if_owed(engine, arn),
        "expected a transition the clock owes {arn:?}: either no connector is stored under \
         that ARN or it has already settled"
    );
}

/// [`release_pending`] for a case that tolerates nothing being owed any more, answering
/// whether a transition was moved — so such a case can skip an assertion about a transient
/// state instead of racing the service's own transition span.
pub fn release_pending_if_owed(engine: &engine::InMemoryEngine, arn: &str) -> bool {
    set_pending_due(engine, arn, SystemTime::now() - ALREADY_DUE)
}

/// [`hold_pending`] for a case that tolerates nothing being owed any more, answering whether
/// a transition was moved.
pub fn hold_pending_if_owed(engine: &engine::InMemoryEngine, arn: &str) -> bool {
    set_pending_due(engine, arn, SystemTime::now() + NOT_DUE_FOR)
}
