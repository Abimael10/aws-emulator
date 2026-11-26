// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `RegisterCertificate` and the `DescribeCertificate` reader
//! that proves what it persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: an operation that REGISTERS certificate material a caller owns
//! records it under the identity the material itself determines, reads every member it
//! reports about the certificate out of that material, and resolves the registered CA that
//! signed it rather than recording a certificate whose signer it cannot name. Here that
//! means `RegisterCertificate` keys the device-certificate store by the SHA-256 digest of
//! the certificate's own DER (the modelled `CertificateId`, `length 64..64, pattern
//! ^(0x)?[a-fA-F0-9]+$`), mints `arn:aws:iot:<region>:<account>:cert/<id>` over that id,
//! reports the `validity` the material carries, resolves the status from `status` and
//! otherwise from the deprecated `setAsActive`, records the certificate "in the same
//! certificate mode as the signing CA" together with that CA's id, and
//! `DescribeCertificate` reports that one record back; material that is not a readable
//! X.509 certificate is `CertificateValidationException` (400), a signer that does not
//! resolve to exactly one registered CA certificate is `CertificateConflictException`
//! (409), a status only IoT's own machinery sets is `CertificateStateException` (406),
//! material already registered is `ResourceAlreadyExistsException` (409) — and a refusal
//! records nothing.
//!
//! The tables span what that rule generalises over rather than one request: both
//! `CertificateMode`s a CA is registered in, the request naming the signing CA and leaving
//! it to be resolved by the certificate's own issuer, every form the status members take on
//! the wire (neither, `setAsActive` either way, `status` alone, and the two together),
//! several subjects and validity periods, the region the call is signed for, and several
//! shapes of material that is not a readable certificate.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    CERTIFICATE_MODE_DEFAULT, CERTIFICATE_MODE_SNI_ONLY, CERTIFICATE_STATUS_ACTIVE,
    CERTIFICATE_STATUS_INACTIVE, CERTIFICATE_STATUS_REVOKED, FIRST_CERTIFICATE_CUSTOMER_VERSION,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use sha2::{Digest, Sha256};
use smithy_server_runtime::MultiServiceRouter;
use x509_cert::der::{DecodePem, Encode};
use x509_cert::Certificate;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";

/// The member a registration carries the device certificate at, which an unreadable-material
/// refusal names (`smithy_query(query="operation",operation="RegisterCertificate")`).
const CERTIFICATE_PEM_MEMBER: &str = "certificatePem";
/// The member a registration names the signing CA certificate at.
const CA_CERTIFICATE_PEM_MEMBER: &str = "caCertificatePem";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// `region`: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `message` of an error body.
    fn message(&self) -> String {
        self.text("message").unwrap_or_default()
    }

    /// The error shape the answer names, as a client reads it: the `x-amzn-errortype`
    /// header up to the `:` that introduces the shape's documentation URI, which this
    /// protocol appends for some shapes and an SDK strips.
    fn code(&self) -> Option<&str> {
        self.error_code
            .as_deref()
            .map(|value| value.split(':').next().unwrap_or(value))
    }

    /// A member of the reported `CertificateDescription`.
    fn described(&self, name: &str) -> Option<&Value> {
        self.body
            .get("certificateDescription")
            .and_then(|description| description.get(name))
    }

    /// A string member of the reported `CertificateDescription`.
    fn described_text(&self, name: &str) -> Option<String> {
        self.described(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// A member of the reported `validity`, in epoch seconds.
    fn validity_seconds(&self, name: &str) -> Option<f64> {
        self.described("validity")
            .and_then(|validity| validity.get(name))
            .and_then(Value::as_f64)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
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

    async fn call(&self, method: &str, region: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
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

    /// `RegisterCACertificate`: `POST /cacertificate`. A `SNI_ONLY` registration carries no
    /// verification certificate and a `DEFAULT` one must carry one, which is the rule that
    /// operation documents.
    async fn register_ca(&self, region: &str, ca_certificate: &str, mode: &str) -> Reply {
        let mut body = json!({
            "caCertificate": ca_certificate,
            "certificateMode": mode,
        });
        if mode == CERTIFICATE_MODE_DEFAULT {
            body["verificationCertificate"] = json!(verification_certificate());
        }
        self.call("POST", region, "/cacertificate?setAsActive=true", Some(&body))
            .await
    }

    /// `RegisterCertificate`: `POST /certificate/register`, with `setAsActive` bound as the
    /// one `@httpQuery` parameter — absent from the query when the case names none — and
    /// everything else in the body.
    async fn register(&self, request: &Registration) -> Reply {
        let path = match request.set_as_active {
            Some(set_as_active) => format!("/certificate/register?setAsActive={set_as_active}"),
            None => "/certificate/register".to_string(),
        };
        let mut body = json!({ "certificatePem": request.certificate_pem });
        if let Some(ca_certificate_pem) = &request.ca_certificate_pem {
            body["caCertificatePem"] = json!(ca_certificate_pem);
        }
        if let Some(status) = request.status {
            body["status"] = json!(status);
        }
        self.call("POST", request.region, &path, Some(&body)).await
    }

    /// `DescribeCertificate`: `GET /certificates/{certificateId}`.
    async fn describe(&self, region: &str, certificate_id: &str) -> Reply {
        self.call("GET", region, &format!("/certificates/{certificate_id}"), None)
            .await
    }

    /// `DescribeCACertificate`: `GET /cacertificate/{certificateId}` — the CA reader, which
    /// a registered DEVICE certificate must not be visible through.
    async fn describe_ca(&self, certificate_id: &str) -> Reply {
        self.call("GET", REGION, &format!("/cacertificate/{certificate_id}"), None)
            .await
    }
}

/// One `RegisterCertificate` request, as a case names it.
struct Registration {
    certificate_pem: String,
    ca_certificate_pem: Option<String>,
    status: Option<&'static str>,
    set_as_active: Option<bool>,
    region: &'static str,
}

impl Registration {
    /// The shortest legal request: the certificate material alone, with the signing CA
    /// left to be resolved by the certificate's own issuer.
    fn of(certificate_pem: &str) -> Self {
        Self {
            certificate_pem: certificate_pem.to_string(),
            ca_certificate_pem: None,
            status: None,
            set_as_active: None,
            region: REGION,
        }
    }

    fn signed_by(mut self, ca_certificate_pem: &str) -> Self {
        self.ca_certificate_pem = Some(ca_certificate_pem.to_string());
        self
    }

    fn with_status(mut self, status: &'static str) -> Self {
        self.status = Some(status);
        self
    }

    fn with_set_as_active(mut self, set_as_active: bool) -> Self {
        self.set_as_active = Some(set_as_active);
        self
    }

    fn in_region(mut self, region: &'static str) -> Self {
        self.region = region;
        self
    }
}

/// A certificate AUTHORITY of the caller's own: the key pair and the self-signed CA
/// certificate, which is what signs the device certificates a case registers.
///
/// Every call generates a fresh key pair, so two authorities under one common name are
/// different material carrying the SAME subject field — which is what leaves a certificate's
/// signer ambiguous when the request names no CA.
struct Authority {
    key: ::rcgen::KeyPair,
    certificate: ::rcgen::Certificate,
}

impl Authority {
    fn new(common_name: &str) -> Self {
        let key = ::rcgen::KeyPair::generate().expect("a key pair is generated");
        let mut params = certificate_params(common_name, 2020, 2040);
        params.is_ca = ::rcgen::IsCa::Ca(::rcgen::BasicConstraints::Unconstrained);
        let certificate = params.self_signed(&key).expect("a self-signed authority");
        Self { key, certificate }
    }

    /// The CA material a caller registers.
    fn pem(&self) -> String {
        self.certificate.pem()
    }

    /// An END-ENTITY certificate signed BY this authority, valid over the period the case
    /// names: a device certificate whose issuer is this authority's subject field, which is
    /// what makes this authority its signer.
    fn signed_device_certificate(
        &self,
        common_name: &str,
        not_before_year: i32,
        not_after_year: i32,
    ) -> String {
        let device_key = ::rcgen::KeyPair::generate().expect("a key pair is generated");
        let mut params = certificate_params(common_name, not_before_year, not_after_year);
        params.is_ca = ::rcgen::IsCa::ExplicitNoCa;
        params
            .signed_by(&device_key, &self.certificate, &self.key)
            .expect("a signed device certificate")
            .pem()
    }
}

/// The parameters both halves of a case's material are built from: one common name as the
/// distinguished name, and the validity period the case names.
fn certificate_params(
    common_name: &str,
    not_before_year: i32,
    not_after_year: i32,
) -> ::rcgen::CertificateParams {
    let mut params =
        ::rcgen::CertificateParams::new(Vec::<String>::new()).expect("certificate parameters");
    let mut subject = ::rcgen::DistinguishedName::new();
    subject.push(::rcgen::DnType::CommonName, common_name);
    params.distinguished_name = subject;
    params.not_before = ::rcgen::date_time_ymd(not_before_year, 1, 1);
    params.not_after = ::rcgen::date_time_ymd(not_after_year, 1, 1);
    params
}

/// A readable certificate for the `verificationCertificate` a `DEFAULT`-mode CA
/// registration must carry.
fn verification_certificate() -> String {
    let key = ::rcgen::KeyPair::generate().expect("a key pair is generated");
    let mut params = certificate_params("verification.example.com", 2020, 2040);
    params.is_ca = ::rcgen::IsCa::ExplicitNoCa;
    params.self_signed(&key).expect("a certificate").pem()
}

/// The `certificateId` a registration of `pem` must report: the SHA-256 digest of the
/// certificate's own DER, hex encoded — computed here from the material the case
/// registered, so the assertion does not read it back out of the answer it is checking.
fn expected_certificate_id(pem: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(parsed(pem).to_der().expect("the certificate re-encodes"));
    ::hex::encode(hasher.finalize())
}

/// The ARN a registered device certificate carries: the `cert/` resource type AWS IoT
/// device certificate ARNs use.
fn expected_certificate_arn(region: &str, certificate_id: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:cert/{certificate_id}")
}

/// The ISSUER of `pem`, as the RFC 4514 string of the certificate's own issuer name: the
/// name a refusal that could not resolve the signer reports.
fn issuer_of(pem: &str) -> String {
    parsed(pem).tbs_certificate.issuer.to_string()
}

/// The X.509 certificate a case's material holds.
fn parsed(pem: &str) -> Certificate {
    Certificate::from_pem(pem.trim().as_bytes()).expect("the case's material is a certificate")
}

/// The epoch second `rcgen::date_time_ymd(year, 1, 1)` names, which is the bound the
/// registered certificate carries and therefore the bound the description must report.
fn january_first(year: i32) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        ::rcgen::date_time_ymd(year, 1, 1).unix_timestamp() as f64
    }
}

/// The message a registration refuses material it cannot read as a certificate with,
/// naming the member it arrived at.
fn unreadable_message(member: &str) -> String {
    format!("Invalid certificate: the {member} field is not a PEM-encoded X.509 certificate")
}

/// The fixed clause every refusal of an unresolvable signer states: the sentence AWS
/// publishes for `CertificateConflictException` — "Unable to verify the CA certificate used
/// to sign the device certificate you are attempting to register"
/// (`smithy_query(query="shape",shape="CertificateConflictException")`).
fn unverifiable_message(reason: &str) -> String {
    format!(
        "Unable to verify the CA certificate used to sign the device certificate you are \
         attempting to register: {reason}"
    )
}

/// The message a registration naming CA material this account has not registered is
/// refused with.
fn unregistered_ca_message() -> String {
    unverifiable_message(&format!(
        "the {CA_CERTIFICATE_PEM_MEMBER} field names a CA certificate this account has not \
         registered"
    ))
}

/// The message a registration whose named CA is not the certificate's issuer is refused
/// with.
fn unmatched_ca_message(issuer: &str) -> String {
    unverifiable_message(&format!(
        "the certificate was issued by {issuer}, which is not the subject field of the CA \
         certificate the {CA_CERTIFICATE_PEM_MEMBER} field names"
    ))
}

/// The message a registration that names no CA is refused with when no registered CA
/// carries the certificate's issuer as its subject field.
fn unknown_ca_message(issuer: &str) -> String {
    unverifiable_message(&format!(
        "this account has registered no CA certificate whose subject field is {issuer}"
    ))
}

/// The message a registration that names no CA is refused with when MORE than one
/// registered CA does: the case `RegisterCertificate` documents as the one where "you must
/// specify the CA certificate that was used to sign the device certificate being
/// registered".
fn ambiguous_ca_message(issuer: &str) -> String {
    unverifiable_message(&format!(
        "this account has registered more than one CA certificate whose subject field is \
         {issuer}, so the {CA_CERTIFICATE_PEM_MEMBER} field has to name the one that signed the \
         certificate"
    ))
}

/// The message a registration naming a status only IoT's own machinery sets is refused
/// with: the sentence every certificate write states an unsettable status in, named after
/// the operation the caller asked for it with.
fn unsettable_status_message(status: &str) -> String {
    format!("Not allowed to update to {status} status using RegisterCertificate call")
}

/// The message a registration of material the account already holds is refused with.
fn already_exists_message(certificate_id: &str) -> String {
    format!("Certificate already exists (certificateId: {certificate_id})")
}

/// The message every reader of a certificate this account holds none of states.
fn missing_message(certificate_id: &str) -> String {
    format!("CertificateId {certificate_id} does not exist")
}

/// One case of the registration table: what the account holds, what the request carries,
/// and the status the record must end up in.
struct Case {
    label: &'static str,
    ca_mode: &'static str,
    ca_subject: &'static str,
    /// Whether the request names the signing CA, or leaves it to the certificate's issuer.
    names_the_ca: bool,
    status: Option<&'static str>,
    set_as_active: Option<bool>,
    not_before_year: i32,
    not_after_year: i32,
    region: &'static str,
    expected_status: &'static str,
}

/// The table: both CA modes, the signer named and resolved by issuer, every form the two
/// status members take on the wire, several validity periods and a region other than the
/// harness default.
fn registration_cases() -> Vec<Case> {
    vec![
        Case {
            label: "SNI_ONLY CA, CA named, neither status nor setAsActive",
            ca_mode: CERTIFICATE_MODE_SNI_ONLY,
            ca_subject: "sni-only-ca.example.com",
            names_the_ca: true,
            status: None,
            set_as_active: None,
            not_before_year: 2021,
            not_after_year: 2031,
            region: REGION,
            expected_status: CERTIFICATE_STATUS_INACTIVE,
        },
        Case {
            label: "SNI_ONLY CA, CA named, setAsActive=true",
            ca_mode: CERTIFICATE_MODE_SNI_ONLY,
            ca_subject: "sni-active-ca.example.com",
            names_the_ca: true,
            status: None,
            set_as_active: Some(true),
            not_before_year: 2022,
            not_after_year: 2032,
            region: REGION,
            expected_status: CERTIFICATE_STATUS_ACTIVE,
        },
        Case {
            label: "DEFAULT CA, CA named, setAsActive=false",
            ca_mode: CERTIFICATE_MODE_DEFAULT,
            ca_subject: "default-ca.example.com",
            names_the_ca: true,
            status: None,
            set_as_active: Some(false),
            not_before_year: 2023,
            not_after_year: 2033,
            region: REGION,
            expected_status: CERTIFICATE_STATUS_INACTIVE,
        },
        Case {
            label: "DEFAULT CA, CA named, status=ACTIVE",
            ca_mode: CERTIFICATE_MODE_DEFAULT,
            ca_subject: "default-active-ca.example.com",
            names_the_ca: true,
            status: Some(CERTIFICATE_STATUS_ACTIVE),
            set_as_active: None,
            not_before_year: 2024,
            not_after_year: 2034,
            region: REGION,
            expected_status: CERTIFICATE_STATUS_ACTIVE,
        },
        Case {
            label: "DEFAULT CA, signer resolved by issuer, status=REVOKED",
            ca_mode: CERTIFICATE_MODE_DEFAULT,
            ca_subject: "issuer-resolved-ca.example.com",
            names_the_ca: false,
            status: Some(CERTIFICATE_STATUS_REVOKED),
            set_as_active: None,
            not_before_year: 2025,
            not_after_year: 2035,
            region: REGION,
            expected_status: CERTIFICATE_STATUS_REVOKED,
        },
        Case {
            label: "SNI_ONLY CA, signer resolved by issuer, status=INACTIVE beside \
                    setAsActive=true",
            ca_mode: CERTIFICATE_MODE_SNI_ONLY,
            ca_subject: "status-wins-ca.example.com",
            names_the_ca: false,
            status: Some(CERTIFICATE_STATUS_INACTIVE),
            set_as_active: Some(true),
            not_before_year: 2026,
            not_after_year: 2036,
            region: REGION,
            expected_status: CERTIFICATE_STATUS_INACTIVE,
        },
        Case {
            label: "DEFAULT CA, CA named, status=ACTIVE, another region",
            ca_mode: CERTIFICATE_MODE_DEFAULT,
            ca_subject: "regional-ca.example.com",
            names_the_ca: true,
            status: Some(CERTIFICATE_STATUS_ACTIVE),
            set_as_active: None,
            not_before_year: 2027,
            not_after_year: 2037,
            region: "eu-west-1",
            expected_status: CERTIFICATE_STATUS_ACTIVE,
        },
    ]
}

/// RULE: a registration records the caller's certificate under the id its own material
/// determines, reports the ARN minted over that id, reads the validity period out of the
/// material, resolves the status from the request and the certificate mode and the CA id
/// from the SIGNING CA — and `DescribeCertificate` reports that one record back.
#[tokio::test]
async fn registered_certificate_is_recorded_under_its_signing_ca() {
    for case in registration_cases() {
        let label = case.label;
        let emulator = Emulator::new();
        let authority = Authority::new(case.ca_subject);
        let ca_pem = authority.pem();

        let registered_ca = emulator
            .register_ca(case.region, &ca_pem, case.ca_mode)
            .await;
        assert_eq!(
            registered_ca.status, 200,
            "case {label:?}: the CA registration must be served, got {registered_ca:?}"
        );
        let ca_certificate_id = registered_ca
            .text("certificateId")
            .unwrap_or_else(|| panic!("case {label:?}: the CA registration reports an id"));

        let device_pem = authority.signed_device_certificate(
            "device.example.com",
            case.not_before_year,
            case.not_after_year,
        );
        let mut request = Registration::of(&device_pem).in_region(case.region);
        if case.names_the_ca {
            request = request.signed_by(&ca_pem);
        }
        if let Some(status) = case.status {
            request = request.with_status(status);
        }
        if let Some(set_as_active) = case.set_as_active {
            request = request.with_set_as_active(set_as_active);
        }

        let registered = emulator.register(&request).await;
        assert_eq!(
            registered.status, 200,
            "case {label:?}: the registration must be served, got {registered:?}"
        );
        let expected_id = expected_certificate_id(&device_pem);
        assert_eq!(
            registered.text("certificateId"),
            Some(expected_id.clone()),
            "case {label:?}: expected the id of the registered material, got {registered:?}"
        );
        assert_eq!(
            registered.text("certificateArn"),
            Some(expected_certificate_arn(case.region, &expected_id)),
            "case {label:?}: expected the ARN minted over that id, got {registered:?}"
        );

        let described = emulator.describe(case.region, &expected_id).await;
        assert_eq!(
            described.status, 200,
            "case {label:?}: the registered certificate must be readable, got {described:?}"
        );
        assert_eq!(
            described.described_text("certificateId"),
            Some(expected_id.clone()),
            "case {label:?}: expected the described id to be the registered one, got \
             {described:?}"
        );
        assert_eq!(
            described.described_text("certificateArn"),
            Some(expected_certificate_arn(case.region, &expected_id)),
            "case {label:?}: expected the described ARN to be the reported one, got {described:?}"
        );
        assert_eq!(
            described.described_text("status"),
            Some(case.expected_status.to_string()),
            "case {label:?}: expected status {:?}, got {described:?}",
            case.expected_status
        );
        assert_eq!(
            described.described_text("certificatePem").as_deref(),
            Some(device_pem.as_str()),
            "case {label:?}: expected the registered material to be reported back, got \
             {described:?}"
        );
        assert_eq!(
            described.described_text("caCertificateId"),
            Some(ca_certificate_id.clone()),
            "case {label:?}: expected the signing CA's id, got {described:?}"
        );
        assert_eq!(
            described.described_text("certificateMode"),
            Some(case.ca_mode.to_string()),
            "case {label:?}: expected the signing CA's mode {:?}, got {described:?}",
            case.ca_mode
        );
        assert_eq!(
            described.described_text("ownedBy"),
            Some(ACCOUNT_ID.to_string()),
            "case {label:?}: expected the emulated account as the owner, got {described:?}"
        );
        assert_eq!(
            described.described("customerVersion").and_then(Value::as_i64),
            Some(i64::from(FIRST_CERTIFICATE_CUSTOMER_VERSION)),
            "case {label:?}: expected the first customer version, got {described:?}"
        );
        assert_eq!(
            described.validity_seconds("notBefore"),
            Some(january_first(case.not_before_year)),
            "case {label:?}: expected the material's own notBefore, got {described:?}"
        );
        assert_eq!(
            described.validity_seconds("notAfter"),
            Some(january_first(case.not_after_year)),
            "case {label:?}: expected the material's own notAfter, got {described:?}"
        );
        assert!(
            described
                .described("generationId")
                .and_then(Value::as_str)
                .is_some_and(|generation_id| !generation_id.is_empty()),
            "case {label:?}: expected a generation id, got {described:?}"
        );
        assert!(
            described
                .described("creationDate")
                .and_then(Value::as_f64)
                .is_some(),
            "case {label:?}: expected a creation date, got {described:?}"
        );

        // A DEVICE certificate is not a CA certificate: the two registries are reached
        // through different URIs and carry different shapes, so the CA reader holds none.
        let as_ca = emulator.describe_ca(&expected_id).await;
        assert_eq!(
            as_ca.status, 404,
            "case {label:?}: a device certificate must not be a CA certificate, got {as_ca:?}"
        );
    }
}

/// One row of the unreadable-material table: material that is not a readable X.509
/// certificate, and the members it is unreadable material AT.
struct Unreadable {
    label: &'static str,
    material: String,
    /// The members the row is offered at. Every row is unreadable material at
    /// `certificatePem`, which is `@required`; a row that is BLANK names no CA at all when
    /// it arrives at `caCertificatePem`, so it is not unreadable material there.
    members: &'static [&'static str],
}

/// The forms material that is not a readable certificate takes: text that is no PEM at
/// all, a PEM block of another type, a truncated certificate block, and whitespace (which
/// satisfies the modelled `length 1..65536` and is still no certificate).
fn unreadable_material(certificate_pem: &str) -> Vec<Unreadable> {
    vec![
        Unreadable {
            label: "not PEM at all",
            material: "this is not a certificate".to_string(),
            members: &[CERTIFICATE_PEM_MEMBER, CA_CERTIFICATE_PEM_MEMBER],
        },
        Unreadable {
            label: "a PEM block of another type",
            material: "-----BEGIN PUBLIC KEY-----\nQUFBQQ==\n-----END PUBLIC KEY-----\n"
                .to_string(),
            members: &[CERTIFICATE_PEM_MEMBER, CA_CERTIFICATE_PEM_MEMBER],
        },
        Unreadable {
            label: "a truncated certificate block",
            material: certificate_pem.chars().take(60).collect::<String>(),
            members: &[CERTIFICATE_PEM_MEMBER, CA_CERTIFICATE_PEM_MEMBER],
        },
        Unreadable {
            label: "whitespace",
            material: " ".to_string(),
            members: &[CERTIFICATE_PEM_MEMBER],
        },
    ]
}

/// RULE: material that is not a readable X.509 certificate is refused
/// `CertificateValidationException` (400) naming the member it arrived at, and the refusal
/// records nothing — whichever of the two members the registration carried it at.
#[tokio::test]
async fn unreadable_material_is_refused_and_records_nothing() {
    let emulator = Emulator::new();
    let authority = Authority::new("unreadable-material-ca.example.com");
    let ca_pem = authority.pem();
    let registered_ca = emulator
        .register_ca(REGION, &ca_pem, CERTIFICATE_MODE_SNI_ONLY)
        .await;
    assert_eq!(
        registered_ca.status, 200,
        "the CA registration must be served, got {registered_ca:?}"
    );
    let device_pem = authority.signed_device_certificate("device.example.com", 2020, 2040);

    for row in unreadable_material(&device_pem) {
        for member in row.members.iter().copied() {
            let label = format!("{} at {member}", row.label);
            let request = if member == CERTIFICATE_PEM_MEMBER {
                Registration::of(&row.material).signed_by(&ca_pem)
            } else {
                Registration::of(&device_pem).signed_by(&row.material)
            };
            let refused = emulator.register(&request).await;
            assert_eq!(
                refused.status, 400,
                "case {label:?}: expected HTTP 400, got {refused:?}"
            );
            assert_eq!(
                refused.code(),
                Some("CertificateValidationException"),
                "case {label:?}: expected CertificateValidationException, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                unreadable_message(member),
                "case {label:?}: expected the refusal to name {member}, got {refused:?}"
            );
        }
    }

    // Nothing was recorded: the one piece of readable material every row carried beside
    // its unreadable half is still unknown to the reader.
    let expected_id = expected_certificate_id(&device_pem);
    let described = emulator.describe(REGION, &expected_id).await;
    assert_eq!(
        described.status, 404,
        "a refused registration must record nothing, got {described:?}"
    );
    assert_eq!(
        described.message(),
        missing_message(&expected_id),
        "a reader of a certificate the account holds none of states the missing sentence, got \
         {described:?}"
    );
}

/// RULE: the signer of a registered certificate is exactly ONE CA certificate of the
/// account's registry — the one the request names, and otherwise the one whose subject
/// field is the certificate's issuer — so every way that resolution has no single answer is
/// `CertificateConflictException` (409) and records nothing.
#[tokio::test]
async fn unresolvable_signing_ca_is_a_conflict_and_records_nothing() {
    let shared_subject = "shared-subject-ca.example.com";

    // A CA the account registered, the certificate it signed, and a SECOND authority under
    // the same subject field that nothing registered yet.
    let registered = Authority::new(shared_subject);
    let registered_pem = registered.pem();
    let device_pem = registered.signed_device_certificate("device.example.com", 2020, 2040);
    let device_issuer = issuer_of(&device_pem);

    let unregistered = Authority::new("unregistered-ca.example.com");
    let unregistered_pem = unregistered.pem();
    let foreign_device_pem =
        unregistered.signed_device_certificate("foreign-device.example.com", 2020, 2040);
    let foreign_issuer = issuer_of(&foreign_device_pem);

    // Each row: what the account holds, the request, and the refusal it must answer with.
    struct Row {
        label: &'static str,
        register_second_ca_under_shared_subject: bool,
        certificate_pem: String,
        ca_certificate_pem: Option<String>,
        expected_message: String,
    }

    let rows = vec![
        Row {
            label: "the named CA is not registered",
            register_second_ca_under_shared_subject: false,
            certificate_pem: foreign_device_pem.clone(),
            ca_certificate_pem: Some(unregistered_pem.clone()),
            expected_message: unregistered_ca_message(),
        },
        Row {
            label: "the named CA did not issue the certificate",
            register_second_ca_under_shared_subject: false,
            certificate_pem: foreign_device_pem.clone(),
            ca_certificate_pem: Some(registered_pem.clone()),
            expected_message: unmatched_ca_message(&foreign_issuer),
        },
        Row {
            label: "no CA named and none registered under the issuer",
            register_second_ca_under_shared_subject: false,
            certificate_pem: foreign_device_pem.clone(),
            ca_certificate_pem: None,
            expected_message: unknown_ca_message(&foreign_issuer),
        },
        Row {
            label: "no CA named and two registered under the issuer",
            register_second_ca_under_shared_subject: true,
            certificate_pem: device_pem.clone(),
            ca_certificate_pem: None,
            expected_message: ambiguous_ca_message(&device_issuer),
        },
    ];

    for row in rows {
        let label = row.label;
        let emulator = Emulator::new();
        let first = emulator
            .register_ca(REGION, &registered_pem, CERTIFICATE_MODE_SNI_ONLY)
            .await;
        assert_eq!(
            first.status, 200,
            "case {label:?}: the CA registration must be served, got {first:?}"
        );
        if row.register_second_ca_under_shared_subject {
            let second = Authority::new(shared_subject);
            let registered_second = emulator
                .register_ca(REGION, &second.pem(), CERTIFICATE_MODE_SNI_ONLY)
                .await;
            assert_eq!(
                registered_second.status, 200,
                "case {label:?}: the second CA registration must be served, got \
                 {registered_second:?}"
            );
        }

        let mut request = Registration::of(&row.certificate_pem);
        if let Some(ca_certificate_pem) = &row.ca_certificate_pem {
            request = request.signed_by(ca_certificate_pem);
        }
        let refused = emulator.register(&request).await;
        assert_eq!(
            refused.status, 409,
            "case {label:?}: expected HTTP 409, got {refused:?}"
        );
        assert_eq!(
            refused.code(),
            Some("CertificateConflictException"),
            "case {label:?}: expected CertificateConflictException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            row.expected_message,
            "case {label:?}: expected the refusal to name why the signer did not resolve, got \
             {refused:?}"
        );

        let expected_id = expected_certificate_id(&row.certificate_pem);
        let described = emulator.describe(REGION, &expected_id).await;
        assert_eq!(
            described.status, 404,
            "case {label:?}: a refused registration must record nothing, got {described:?}"
        );
    }
}

/// RULE: a status only AWS IoT's own machinery sets is one no caller may ask for, so a
/// registration naming it is `CertificateStateException` (406) and records nothing, while
/// every status a caller may ask for is recorded as asked.
#[tokio::test]
async fn status_iot_sets_itself_cannot_be_registered() {
    let emulator = Emulator::new();
    let authority = Authority::new("internal-status-ca.example.com");
    let ca_pem = authority.pem();
    let registered_ca = emulator
        .register_ca(REGION, &ca_pem, CERTIFICATE_MODE_SNI_ONLY)
        .await;
    assert_eq!(
        registered_ca.status, 200,
        "the CA registration must be served, got {registered_ca:?}"
    );

    // The statuses `UpdateCertificate$newStatus` documents as "used internally by IoT. They
    // are not intended for developer use"
    // (`smithy_query(query="operation",operation="UpdateCertificate")`).
    for status in ["PENDING_ACTIVATION", "PENDING_TRANSFER"] {
        let device_pem =
            authority.signed_device_certificate(&format!("{status}-device.example.com"), 2020, 2040);
        let refused = emulator
            .register(
                &Registration::of(&device_pem)
                    .signed_by(&ca_pem)
                    .with_status(status),
            )
            .await;
        assert_eq!(
            refused.status, 406,
            "case {status:?}: expected HTTP 406, got {refused:?}"
        );
        assert_eq!(
            refused.code(),
            Some("CertificateStateException"),
            "case {status:?}: expected CertificateStateException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            unsettable_status_message(status),
            "case {status:?}: expected the unsettable-status sentence, got {refused:?}"
        );

        let described = emulator
            .describe(REGION, &expected_certificate_id(&device_pem))
            .await;
        assert_eq!(
            described.status, 404,
            "case {status:?}: a refused registration must record nothing, got {described:?}"
        );
    }
}

/// RULE: `caCertificatePem` is the member that DISAMBIGUATES the signer, so material that
/// is blank names no CA at all and the signer is resolved by the certificate's own issuer —
/// the same reading a blank `verificationCertificate` gets on the CA registration.
#[tokio::test]
async fn blank_ca_material_names_no_signing_ca() {
    let emulator = Emulator::new();
    let authority = Authority::new("blank-ca-material-ca.example.com");
    let ca_pem = authority.pem();
    let registered_ca = emulator
        .register_ca(REGION, &ca_pem, CERTIFICATE_MODE_SNI_ONLY)
        .await;
    assert_eq!(
        registered_ca.status, 200,
        "the CA registration must be served, got {registered_ca:?}"
    );
    let ca_certificate_id = registered_ca
        .text("certificateId")
        .expect("the CA registration reports an id");

    // Whitespace only: `CertificatePem` is modelled `length 1..65536`, so these are legal
    // values of the member and carry no CA.
    for (index, blank) in ["   ", "\n\n"].into_iter().enumerate() {
        let label = format!("blank caCertificatePem {blank:?}");
        let device_pem =
            authority.signed_device_certificate(&format!("blank-{index}.example.com"), 2020, 2040);
        let registered = emulator
            .register(&Registration::of(&device_pem).signed_by(blank))
            .await;
        assert_eq!(
            registered.status, 200,
            "case {label:?}: the registration must be served, got {registered:?}"
        );
        let expected_id = expected_certificate_id(&device_pem);
        assert_eq!(
            registered.text("certificateId"),
            Some(expected_id.clone()),
            "case {label:?}: expected the id of the registered material, got {registered:?}"
        );
        let described = emulator.describe(REGION, &expected_id).await;
        assert_eq!(
            described.described_text("caCertificateId"),
            Some(ca_certificate_id.clone()),
            "case {label:?}: expected the signer resolved by the certificate's issuer, got \
             {described:?}"
        );
    }
}
/// RULE: a certificate's identity is its own material, so registering material the account
/// already holds is a collision — `ResourceAlreadyExistsException` (409) reporting the
/// record that exists — and the record it reports is left exactly as the first registration
/// wrote it.
#[tokio::test]
async fn repeat_registration_of_one_certificate_is_a_collision() {
    let emulator = Emulator::new();
    let authority = Authority::new("repeat-registration-ca.example.com");
    let ca_pem = authority.pem();
    let registered_ca = emulator
        .register_ca(REGION, &ca_pem, CERTIFICATE_MODE_SNI_ONLY)
        .await;
    assert_eq!(
        registered_ca.status, 200,
        "the CA registration must be served, got {registered_ca:?}"
    );
    let device_pem = authority.signed_device_certificate("device.example.com", 2020, 2040);
    let expected_id = expected_certificate_id(&device_pem);

    let first = emulator
        .register(
            &Registration::of(&device_pem)
                .signed_by(&ca_pem)
                .with_status(CERTIFICATE_STATUS_INACTIVE),
        )
        .await;
    assert_eq!(
        first.status, 200,
        "the first registration must be served, got {first:?}"
    );

    // A second registration of the same material, asking for another status: the material
    // names the record that exists, so nothing is written and nothing moves.
    let repeated = emulator
        .register(
            &Registration::of(&device_pem)
                .signed_by(&ca_pem)
                .with_status(CERTIFICATE_STATUS_ACTIVE),
        )
        .await;
    assert_eq!(
        repeated.status, 409,
        "expected HTTP 409 for material already registered, got {repeated:?}"
    );
    assert_eq!(
        repeated.code(),
        Some("ResourceAlreadyExistsException"),
        "expected ResourceAlreadyExistsException, got {repeated:?}"
    );
    assert_eq!(
        repeated.message(),
        already_exists_message(&expected_id),
        "expected the certificate family's collision sentence, got {repeated:?}"
    );
    assert_eq!(
        repeated.text("resourceId"),
        Some(expected_id.clone()),
        "expected the refusal to report the existing record's id, got {repeated:?}"
    );
    assert_eq!(
        repeated.text("resourceArn"),
        Some(expected_certificate_arn(REGION, &expected_id)),
        "expected the refusal to report the existing record's ARN, got {repeated:?}"
    );

    let described = emulator.describe(REGION, &expected_id).await;
    assert_eq!(
        described.described_text("status"),
        Some(CERTIFICATE_STATUS_INACTIVE.to_string()),
        "expected the first registration's status to stand, got {described:?}"
    );
    assert_eq!(
        described.described("customerVersion").and_then(Value::as_i64),
        Some(i64::from(FIRST_CERTIFICATE_CUSTOMER_VERSION)),
        "expected the refused registration to move no customer version, got {described:?}"
    );
}
