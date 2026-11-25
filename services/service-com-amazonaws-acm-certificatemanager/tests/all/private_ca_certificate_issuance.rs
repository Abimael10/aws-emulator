// SPDX-License-Identifier: CC0-1.0

//! Issuance through a PRIVATE certificate authority, end to end over the wire: ACM's
//! `POST /` with `X-Amz-Target: CertificateManager.<Op>` beside ACM Private CA's own
//! `ACMPrivateCA.<Op>`, both mounted on ONE engine exactly as `core/router` assembles
//! them.
//!
//! RULE under test: a certificate requested with a `CertificateAuthorityArn` is issued BY
//! that private certificate authority, not by ACM's public one. So its `Type` is `PRIVATE`
//! ("the Amazon Resource Name (ARN) of the private certificate authority (CA) that issued
//! the certificate", `CertificateDetail$CertificateAuthorityArn`), the certificate it
//! reports is the one that CA signed — chaining up through the CA's own certificate, under
//! the key pair ACM minted for it — and `ExportCertificate` hands all three out, since
//! "exports a private certificate issued by a private certificate authority (CA) or a
//! public certificate for use anywhere" (`ExportCertificate` doc) makes the opt-in a
//! PUBLIC certificate's alone. A CA ACM cannot issue through settles the request `FAILED`
//! with the `FailureReason` naming the CA's answer.
//!
//! The tables span what the rule generalises over: the `KeyAlgorithm` of the key pair ACM
//! mints (the RSA sizes and the elliptic curves the provider offers), one covered name as
//! against several and a wildcard, and each answer a CA can give a request — issued, a
//! usage mode ACM does not issue through, and a CA that is not active yet.
//!
//! Nothing here asserts on the wall clock. The private-CA side is polled for the condition
//! each step waits on, and ACM's own owed issuance is materialised by moving the instant
//! the emulator itself stored into the past, as every other suite here does.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_lc_rs::signature::{
    UnparsedPublicKey, VerificationAlgorithm, ECDSA_P256_SHA256_ASN1, ECDSA_P384_SHA384_ASN1,
    ECDSA_P521_SHA512_ASN1, RSA_PKCS1_2048_8192_SHA256,
};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use x509_cert::der::{DecodePem, Encode};
use x509_cert::Certificate;

use crate::certificate_lifecycle::{call, describe, error_code, error_message, REGION};
use crate::certificate_validation_state_machine::make_validation_due_now;

/// The passphrase every export below assigns to the private key: inside the modelled
/// `@length 4..128` and made of the characters `ExportCertificateRequest$Passphrase`
/// admits ("any ASCII character except #, $, or %").
const PASSPHRASE: &[u8] = b"Sim2RealExportPass9";

/// The `TemplateArn` of the certificate definition a ROOT CA's own certificate is issued
/// under, as Private CA publishes it.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// How long a poll loop waits for a condition the emulator owes the clock before it gives
/// up. Generous: the spans themselves are sub-second, and a loaded host may take far
/// longer to get there than a fast one.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// An emulator with ACM and ACM Private CA mounted on one engine.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_acmpca_acmprivateca::EmulatedService::new(Arc::clone(&engine)),
        );
        Self { router, engine }
    }

    /// Call one ACM operation, signed for [`REGION`].
    async fn acm(&self, operation: &str, body: &Value) -> (u16, Value) {
        call(&self.router, operation, body).await
    }

    /// Call one Private CA operation, signed for [`REGION`] so the CA it mints lives in the
    /// same region the ACM certificates do.
    async fn pca(&self, operation: &str, body: &Value) -> (u16, Value) {
        let host = format!("acm-pca.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("host", &host)
            .header("x-amz-target", format!("ACMPrivateCA.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTTESTTEST/20260101/{REGION}/acm-pca/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Call one Private CA operation, requiring success.
    async fn pca_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.pca(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to answer 200, got {status} {parsed}"
        );
        parsed
    }

    /// A string member of a response.
    fn member(case: &str, response: &Value, member: &str) -> String {
        response[member]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no {member} in {response}"))
            .to_string()
    }

    /// Create a ROOT CA with `usage_mode`, carry it through the documented activation
    /// procedure and answer (its ARN, the certificate it was activated with).
    ///
    /// Every step is a service call: the CA publishes a certification request, the CA signs
    /// its own root certificate under the root-CA template, and that certificate is
    /// imported — which is what makes the CA `ACTIVE`.
    async fn active_root_ca(&self, case: &str, common_name: &str, usage_mode: &str) -> (String, String) {
        let arn = self.pending_root_ca(case, common_name, usage_mode).await;
        let certificate = self.activate(case, &arn).await;
        (arn, certificate)
    }

    /// Create a ROOT CA with `usage_mode` and poll it to the status in which it publishes
    /// its certification request, answering its ARN.
    async fn pending_root_ca(&self, case: &str, common_name: &str, usage_mode: &str) -> String {
        let created = self
            .pca_ok(
                case,
                "CreateCertificateAuthority",
                &json!({
                    "CertificateAuthorityConfiguration": {
                        "KeyAlgorithm": "RSA_2048",
                        "SigningAlgorithm": "SHA256WITHRSA",
                        "Subject": {"CommonName": common_name},
                    },
                    "CertificateAuthorityType": "ROOT",
                    "UsageMode": usage_mode,
                }),
            )
            .await;
        let arn = Self::member(case, &created, "CertificateAuthorityArn");
        self.poll_ca_status(case, &arn, "PENDING_CERTIFICATE").await;
        arn
    }

    /// Activate the CA `arn` with the self-signed root certificate it signs for its own
    /// certification request, answering that certificate.
    async fn activate(&self, case: &str, arn: &str) -> String {
        let csr = Self::member(
            case,
            &self
                .pca_ok(
                    case,
                    "GetCertificateAuthorityCsr",
                    &json!({"CertificateAuthorityArn": arn}),
                )
                .await,
            "Csr",
        );
        let issued = self
            .pca_ok(
                case,
                "IssueCertificate",
                &json!({
                    "CertificateAuthorityArn": arn,
                    "Csr": aws_smithy_types::base64::encode(csr.as_bytes()),
                    "SigningAlgorithm": "SHA256WITHRSA",
                    "TemplateArn": ROOT_CA_TEMPLATE,
                    "Validity": {"Value": 10, "Type": "YEARS"},
                }),
            )
            .await;
        let certificate_arn = Self::member(case, &issued, "CertificateArn");
        let certificate = self.poll_issued_certificate(case, arn, &certificate_arn).await;
        self.pca_ok(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({
                "CertificateAuthorityArn": arn,
                "Certificate": aws_smithy_types::base64::encode(certificate.as_bytes()),
            }),
        )
        .await;
        certificate
    }

    /// Look at the CA `arn` until `expected` is the status it reports, failing the test if
    /// [`POLL_DEADLINE`] passes first.
    async fn poll_ca_status(&self, case: &str, arn: &str, expected: &str) {
        let started = Instant::now();
        let mut last = Value::Null;
        while started.elapsed() < POLL_DEADLINE {
            last = self
                .pca_ok(
                    case,
                    "DescribeCertificateAuthority",
                    &json!({"CertificateAuthorityArn": arn}),
                )
                .await;
            if last["CertificateAuthority"]["Status"].as_str() == Some(expected) {
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        panic!("case {case:?}: {arn} never reached {expected}, last read {last}");
    }

    /// Look for the certificate `certificate_arn` the CA `arn` issued until it is served,
    /// answering its PEM; the CA completes an issuance on the clock, so the read is
    /// repeated until it does.
    async fn poll_issued_certificate(
        &self,
        case: &str,
        arn: &str,
        certificate_arn: &str,
    ) -> String {
        let started = Instant::now();
        let mut last = Value::Null;
        while started.elapsed() < POLL_DEADLINE {
            let (status, response) = self
                .pca(
                    "GetCertificate",
                    &json!({
                        "CertificateAuthorityArn": arn,
                        "CertificateArn": certificate_arn,
                    }),
                )
                .await;
            if status == 200 {
                return Self::member(case, &response, "Certificate");
            }
            last = response;
            std::thread::sleep(POLL_INTERVAL);
        }
        panic!("case {case:?}: {certificate_arn} was never served, last answer {last}");
    }

    /// Request one certificate through ACM, answering its ARN.
    async fn request_certificate(&self, case: &str, body: &Value) -> String {
        let (status, response) = self.acm("RequestCertificate", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: RequestCertificate must succeed, got {status} {response}"
        );
        Self::member(case, &response, "CertificateArn")
    }

    /// Export the certificate `arn`, answering the parsed response.
    async fn export(&self, arn: &str) -> (u16, Value) {
        self.acm(
            "ExportCertificate",
            &json!({
                "CertificateArn": arn,
                "Passphrase": aws_smithy_types::base64::encode(PASSPHRASE),
            }),
        )
        .await
    }
}

/// RULE: a certificate requested through an ACTIVE private CA is `PRIVATE`, is the
/// certificate that CA signed, and exports with its CA-led chain and its own key —
/// whichever `KeyAlgorithm` the key pair was minted for.
#[tokio::test]
async fn a_private_ca_certificate_is_issued_by_that_ca_and_exports_with_its_key() {
    // The rows span the `KeyAlgorithm` members the provider mints a key pair for — both
    // families and three RSA sizes — and the name sets a certificate can cover: one name,
    // several, and a wildcard. Each row's verification algorithm is the one that checks a
    // signature made with the CA's RSA_2048 key, so it is the same for every row: it is
    // the LEAF's key that varies here.
    let cases: &[(&str, &str, &[&str])] = &[
        ("RSA_2048, one name", "RSA_2048", &["one.private.example.com"]),
        (
            "RSA_3072, two names",
            "RSA_3072",
            &["two.private.example.com", "www.two.private.example.com"],
        ),
        ("RSA_4096, one name", "RSA_4096", &["four.private.example.com"]),
        (
            "EC_prime256v1, a wildcard and its apex",
            "EC_prime256v1",
            &["*.p256.private.example.com", "p256.private.example.com"],
        ),
        ("EC_secp384r1, one name", "EC_secp384r1", &["p384.private.example.com"]),
        ("EC_secp521r1, one name", "EC_secp521r1", &["p521.private.example.com"]),
    ];

    let emulator = Emulator::new();
    let (ca_arn, ca_certificate) = emulator
        .active_root_ca("fixture", "private-ca.example.com", "GENERAL_PURPOSE")
        .await;

    // The chain an exported certificate leads with has to be the CA's own certificate as
    // Private CA serves it, byte for byte.
    let served_ca_certificate = Emulator::member(
        "fixture",
        &emulator
            .pca_ok(
                "fixture",
                "GetCertificateAuthorityCertificate",
                &json!({"CertificateAuthorityArn": &ca_arn}),
            )
            .await,
        "Certificate",
    );
    assert_eq!(
        served_ca_certificate, ca_certificate,
        "case fixture: the CA serves the certificate it was activated with"
    );

    for (case, key_algorithm, names) in cases {
        let (domain_name, alternative_names) = names.split_first().expect("a row names a domain");
        let mut body = json!({
            "DomainName": domain_name,
            "CertificateAuthorityArn": &ca_arn,
            "KeyAlgorithm": key_algorithm,
        });
        if !alternative_names.is_empty() {
            body["SubjectAlternativeNames"] = json!(alternative_names);
        }
        let arn = emulator.request_certificate(case, &body).await;

        // The issuance is owed, not immediate: a caller reading straight back still sees
        // the status a request enters, and the material is not served yet.
        let pending = describe(&emulator.router, &arn).await;
        assert_eq!(
            pending["Status"].as_str(),
            Some("PENDING_VALIDATION"),
            "case {case:?}: a request's own status is PENDING_VALIDATION, got {pending}"
        );
        assert_eq!(
            pending["Type"].as_str(),
            Some("PRIVATE"),
            "case {case:?}: a request naming a private CA is PRIVATE from the start, got {pending}"
        );
        let (export_status, export_body) = emulator.export(&arn).await;
        assert_eq!(
            export_status, 400,
            "case {case:?}: a certificate that is not issued yet exports nothing, got \
             {export_status} {export_body}"
        );
        assert_eq!(
            error_code(&export_body),
            "ResourceNotFoundException",
            "case {case:?}: expected no issued certificate under the ARN, got {export_body}"
        );

        // Then the issuance the clock owes falls due.
        make_validation_due_now(&emulator.engine);
        let detail = describe(&emulator.router, &arn).await;
        assert_eq!(
            detail["Status"].as_str(),
            Some("ISSUED"),
            "case {case:?}: expected ISSUED, got {detail}"
        );
        assert_eq!(
            detail["Type"].as_str(),
            Some("PRIVATE"),
            "case {case:?}: expected Type PRIVATE, got {detail}"
        );
        assert_eq!(
            detail["CertificateAuthorityArn"].as_str(),
            Some(ca_arn.as_str()),
            "case {case:?}: expected the issuing CA echoed, got {detail}"
        );
        assert_eq!(
            detail["Issuer"].as_str(),
            Some("private-ca.example.com"),
            "case {case:?}: expected the issuing CA's own name as the Issuer, got {detail}"
        );
        assert_eq!(
            detail["SignatureAlgorithm"].as_str(),
            Some("SHA256WITHRSA"),
            "case {case:?}: expected the algorithm the CA signs with, got {detail}"
        );
        assert!(
            detail["FailureReason"].is_null(),
            "case {case:?}: an issued certificate carries no FailureReason, got {detail}"
        );

        // The export: the certificate, the CA-led chain and the key, all three.
        let (status, exported) = emulator.export(&arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a private certificate exports, got {status} {exported}"
        );
        let certificate_pem = Emulator::member(case, &exported, "Certificate");
        let chain_pem = Emulator::member(case, &exported, "CertificateChain");
        let private_key_pem = Emulator::member(case, &exported, "PrivateKey");

        // The chain leads with the issuing CA's certificate as Private CA serves it.
        assert!(
            chain_pem.starts_with(&served_ca_certificate),
            "case {case:?}: expected the chain to lead with the CA's own certificate \
             {served_ca_certificate:?}, got {chain_pem:?}"
        );

        // The leaf is the certificate ACM reports, it covers every name the request named,
        // and its signature verifies against the CA certificate's public key.
        let leaf = Certificate::from_pem(certificate_pem.as_bytes())
            .unwrap_or_else(|error| panic!("case {case:?}: the exported leaf is X.509: {error}"));
        let served = Emulator::member(
            case,
            &{
                let (status, response) = emulator
                    .acm("GetCertificate", &json!({ "CertificateArn": &arn }))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: GetCertificate serves a private certificate, got \
                     {status} {response}"
                );
                response
            },
            "Certificate",
        );
        assert_eq!(
            served, certificate_pem,
            "case {case:?}: GetCertificate and ExportCertificate serve one certificate"
        );
        let subject = leaf.tbs_certificate.subject.to_string();
        assert!(
            subject.contains(domain_name),
            "case {case:?}: expected the leaf's subject to name {domain_name}, got {subject}"
        );
        verify_signed_by(case, &leaf, &served_ca_certificate);

        // The exported key is the key of the pair the certificate certifies, and it is
        // readable only with the passphrase this request assigned to it.
        let public_key_der = decrypted_public_key(case, &private_key_pem, key_algorithm);
        let certified_key_der = leaf
            .tbs_certificate
            .subject_public_key_info
            .to_der()
            .expect("the leaf's SubjectPublicKeyInfo encodes");
        assert_eq!(
            public_key_der, certified_key_der,
            "case {case:?}: the exported key's public half must be the one the certificate \
             carries"
        );

        // "Indicates whether the certificate has been exported"
        // (`AcmCertificateMetadata$Exported`): the export is what makes that true.
        let after = describe(&emulator.router, &arn).await;
        assert_eq!(
            after["Status"].as_str(),
            Some("ISSUED"),
            "case {case:?}: an export leaves the certificate issued, got {after}"
        );
        assert_eq!(
            exported_flag(&emulator, &arn).await,
            Some(true),
            "case {case:?}: expected the record to report the export, got {after}"
        );
    }
}

/// RULE: a wrong passphrase does not read the exported key back, and two exports of one
/// certificate are two different files — the encryption is per export.
#[tokio::test]
async fn an_exported_key_is_readable_only_with_the_passphrase_it_was_assigned() {
    let emulator = Emulator::new();
    let (ca_arn, _) = emulator
        .active_root_ca("fixture", "passphrase-ca.example.com", "GENERAL_PURPOSE")
        .await;
    let arn = emulator
        .request_certificate(
            "fixture",
            &json!({
                "DomainName": "passphrase.private.example.com",
                "CertificateAuthorityArn": &ca_arn,
            }),
        )
        .await;
    make_validation_due_now(&emulator.engine);

    let (status, first) = emulator.export(&arn).await;
    assert_eq!(status, 200, "case fixture: the export succeeds, got {first}");
    let (status, second) = emulator.export(&arn).await;
    assert_eq!(status, 200, "case fixture: the export succeeds again, got {second}");
    let first_key = Emulator::member("fixture", &first, "PrivateKey");
    let second_key = Emulator::member("fixture", &second, "PrivateKey");
    assert_ne!(
        first_key, second_key,
        "case two exports of one certificate: each is encrypted on its own, so the two \
         files differ"
    );
    assert_eq!(
        Emulator::member("fixture", &first, "Certificate"),
        Emulator::member("fixture", &second, "Certificate"),
        "case two exports of one certificate: the certificate itself is the same one"
    );

    // Each export is read back by the passphrase it was assigned, and by no other.
    assert!(
        decrypt_exported_key(&first_key, PASSPHRASE).is_some(),
        "case the passphrase the export assigned: expected the key to decrypt"
    );
    for wrong in [
        b"not-the-passphrase".as_slice(),
        b"Sim2RealExportPass".as_slice(),
        b"".as_slice(),
    ] {
        assert!(
            decrypt_exported_key(&first_key, wrong).is_none(),
            "case {wrong:?}: a passphrase the export did not assign must not read the key"
        );
    }
    // The second export's own file is read by the same passphrase, so the first file's
    // salt is not what made the first one readable.
    assert!(
        decrypt_exported_key(&second_key, PASSPHRASE).is_some(),
        "case the second export: expected the key to decrypt"
    );
}

/// RULE: a CA ACM does not issue through settles the request `FAILED` with the
/// `FailureReason` naming that answer, and such a certificate exports nothing.
#[tokio::test]
async fn a_certificate_authority_acm_cannot_issue_through_fails_the_request() {
    let emulator = Emulator::new();

    // An ACTIVE short-lived-certificate CA: ACM issues no certificate through one.
    let (short_lived, _) = emulator
        .active_root_ca("short lived", "short-lived-ca.example.com", "SHORT_LIVED_CERTIFICATE")
        .await;
    // A CA that is not active yet: it holds no certificate to sign with.
    let pending = emulator
        .pending_root_ca("pending", "pending-ca.example.com", "GENERAL_PURPOSE")
        .await;
    // The control: the identical request against a general-purpose, active CA.
    let (general_purpose, _) = emulator
        .active_root_ca("general purpose", "general-ca.example.com", "GENERAL_PURPOSE")
        .await;

    // Each row: what it is, the CA to request through, the terminal status and the
    // `FailureReason` beside it.
    let cases: &[(&str, &String, &str, Option<&str>)] = &[
        (
            "an active SHORT_LIVED_CERTIFICATE CA",
            &short_lived,
            "FAILED",
            Some("PCA_NOT_SUPPORTED_VIA_ACM"),
        ),
        (
            "a CA still waiting for its own certificate",
            &pending,
            "FAILED",
            Some("PCA_INVALID_STATE"),
        ),
        (
            "an active GENERAL_PURPOSE CA",
            &general_purpose,
            "ISSUED",
            None,
        ),
    ];

    for (case, ca_arn, expected_status, expected_reason) in cases {
        let arn = emulator
            .request_certificate(
                case,
                &json!({
                    "DomainName": "usage.private.example.com",
                    "CertificateAuthorityArn": ca_arn,
                }),
            )
            .await;
        make_validation_due_now(&emulator.engine);
        let detail = describe(&emulator.router, &arn).await;
        assert_eq!(
            detail["Status"].as_str(),
            Some(*expected_status),
            "case {case:?}: expected {expected_status}, got {detail}"
        );
        assert_eq!(
            detail["FailureReason"].as_str(),
            *expected_reason,
            "case {case:?}: expected FailureReason {expected_reason:?}, got {detail}"
        );
        // Every one of them is a PRIVATE certificate: the type is the provenance the
        // request named, whatever the authority then answered.
        assert_eq!(
            detail["Type"].as_str(),
            Some("PRIVATE"),
            "case {case:?}: expected Type PRIVATE, got {detail}"
        );

        let (status, body) = emulator.export(&arn).await;
        let expected_export_code = if *expected_status == "ISSUED" {
            None
        } else {
            Some("ResourceNotFoundException")
        };
        match expected_export_code {
            Some(code) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: a certificate that was never issued exports nothing, got \
                     {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    code,
                    "case {case:?}: expected {code}, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    format!("Could not find an issued certificate {arn}"),
                    "case {case:?}: expected the sentence for an ARN under which no issued \
                     certificate stands, got {body}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: the issued control exports, got {status} {body}"
            ),
        }
    }
}

/// RULE: the certificate a private CA signs for ACM is the CA's OWN issuance — it is
/// listed under that CA and served by its `GetCertificate`, so the ACM certificate is
/// backed by a certificate the owning service holds.
#[tokio::test]
async fn the_issuing_authority_holds_the_certificate_it_signed_for_acm() {
    let emulator = Emulator::new();
    let (ca_arn, _) = emulator
        .active_root_ca("fixture", "listing-ca.example.com", "GENERAL_PURPOSE")
        .await;
    let arn = emulator
        .request_certificate(
            "fixture",
            &json!({
                "DomainName": "listed.private.example.com",
                "CertificateAuthorityArn": &ca_arn,
            }),
        )
        .await;
    make_validation_due_now(&emulator.engine);
    let detail = describe(&emulator.router, &arn).await;
    let serial = detail["Serial"]
        .as_str()
        .unwrap_or_else(|| panic!("case fixture: expected a Serial on {detail}"))
        .to_string();
    let exported = {
        let (status, body) = emulator.export(&arn).await;
        assert_eq!(status, 200, "case fixture: the export succeeds, got {body}");
        Emulator::member("fixture", &body, "Certificate")
    };

    // The CA this certificate names is the one that answers for it: the certificate ARN
    // is the CA's own, and its `GetCertificate` serves the very bytes ACM exported.
    let certificate_arn = format!("{ca_arn}/certificate/{serial}");
    let served = emulator
        .poll_issued_certificate("fixture", &ca_arn, &certificate_arn)
        .await;
    assert_eq!(
        served, exported,
        "case fixture: the issuing CA serves the certificate ACM exported"
    );
}

/// Whether the stored record reports that the certificate has been exported, read through
/// the one projection that carries the member (`SearchCertificates`).
async fn exported_flag(emulator: &Emulator, arn: &str) -> Option<bool> {
    let (status, response) = emulator
        .acm(
            "SearchCertificates",
            &json!({ "Filter": { "CertificateArn": arn } }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {arn}: SearchCertificates must succeed, got {status} {response}"
    );
    response["Results"]
        .as_array()
        .and_then(|results| results.first().cloned())
        .and_then(|result| {
            result["CertificateMetadata"]["AcmCertificateMetadata"]["Exported"].as_bool()
        })
}

/// The PKCS#8 one exported `PrivateKey` frames, decrypted with `passphrase`, or `None`
/// when that passphrase does not read it.
///
/// The PEM has to be the encrypted PKCS#8 an encrypted private key is published as
/// (RFC 7468 section 11); nothing but the passphrase then reads it back.
fn decrypt_exported_key(private_key_pem: &str, passphrase: &[u8]) -> Option<pkcs8::SecretDocument> {
    let (label, document) =
        pkcs8::SecretDocument::from_pem(private_key_pem).expect("the exported key is PEM");
    assert_eq!(
        label, "ENCRYPTED PRIVATE KEY",
        "an exported private key is an encrypted PKCS#8 (RFC 7468 section 11)"
    );
    let encrypted = pkcs8::EncryptedPrivateKeyInfo::try_from(document.as_bytes())
        .expect("the exported key is an EncryptedPrivateKeyInfo");
    encrypted.decrypt(passphrase).ok()
}

/// The `SubjectPublicKeyInfo` DER of the key pair an exported `PrivateKey` holds, read by
/// decrypting it with [`PASSPHRASE`] and asking the cryptography provider for the public
/// half — so the comparison against the certificate is made by something other than the
/// code under test.
fn decrypted_public_key(case: &str, private_key_pem: &str, key_algorithm: &str) -> Vec<u8> {
    let decrypted = decrypt_exported_key(private_key_pem, PASSPHRASE)
        .unwrap_or_else(|| panic!("case {case:?}: the passphrase must decrypt the exported key"));
    let pkcs8 = decrypted.as_bytes();
    match key_algorithm {
        "RSA_2048" | "RSA_3072" | "RSA_4096" => {
            let key = aws_lc_rs::signature::RsaKeyPair::from_pkcs8(pkcs8)
                .unwrap_or_else(|error| panic!("case {case:?}: an RSA PKCS#8: {error}"));
            let der: aws_lc_rs::encoding::PublicKeyX509Der =
                aws_lc_rs::encoding::AsDer::as_der(aws_lc_rs::signature::KeyPair::public_key(
                    &key,
                ))
                .expect("the public key encodes");
            der.as_ref().to_vec()
        }
        _ => {
            let algorithm = ecdsa_signing_algorithm(key_algorithm);
            let key = aws_lc_rs::signature::EcdsaKeyPair::from_pkcs8(algorithm, pkcs8)
                .unwrap_or_else(|error| panic!("case {case:?}: an ECDSA PKCS#8: {error}"));
            let der: aws_lc_rs::encoding::PublicKeyX509Der =
                aws_lc_rs::encoding::AsDer::as_der(aws_lc_rs::signature::KeyPair::public_key(
                    &key,
                ))
                .expect("the public key encodes");
            der.as_ref().to_vec()
        }
    }
}

/// The ECDSA signing algorithm one `KeyAlgorithm` curve names, for reading a key of that
/// curve back.
fn ecdsa_signing_algorithm(
    key_algorithm: &str,
) -> &'static aws_lc_rs::signature::EcdsaSigningAlgorithm {
    match key_algorithm {
        "EC_prime256v1" => &aws_lc_rs::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        "EC_secp384r1" => &aws_lc_rs::signature::ECDSA_P384_SHA384_ASN1_SIGNING,
        "EC_secp521r1" => &aws_lc_rs::signature::ECDSA_P521_SHA512_ASN1_SIGNING,
        other => panic!("no elliptic curve named {other}"),
    }
}

/// Assert that `leaf` carries a signature made by the key in the certificate
/// `issuer_certificate_pem` — the relationship a relying party checks when it builds the
/// chain, verified here with the cryptography provider rather than with anything the
/// service wrote.
fn verify_signed_by(case: &str, leaf: &Certificate, issuer_certificate_pem: &str) {
    let issuer = Certificate::from_pem(issuer_certificate_pem.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: the CA certificate is X.509: {error}"));
    let signed = leaf
        .tbs_certificate
        .to_der()
        .expect("the signed portion encodes");
    let signature = leaf.signature.raw_bytes();
    let public_key = issuer
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .raw_bytes();
    let algorithm: &dyn VerificationAlgorithm =
        match leaf.signature_algorithm.oid.to_string().as_str() {
            // sha256WithRSAEncryption (RFC 8017 appendix C).
            "1.2.840.113549.1.1.11" => &RSA_PKCS1_2048_8192_SHA256,
            // The ECDSA identifiers of RFC 5758 section 3.2.
            "1.2.840.10045.4.3.2" => &ECDSA_P256_SHA256_ASN1,
            "1.2.840.10045.4.3.3" => &ECDSA_P384_SHA384_ASN1,
            "1.2.840.10045.4.3.4" => &ECDSA_P521_SHA512_ASN1,
            other => panic!("case {case:?}: no verifier for signature algorithm {other}"),
        };
    UnparsedPublicKey::new(algorithm, public_key)
        .verify(&signed, signature)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the leaf must verify against the CA's public key: {error}")
        });
}

/// The instant a certificate's validity window opens, as the record reports it: proof the
/// window on the ACM record is the one the certificate itself carries.
#[tokio::test]
async fn the_reported_validity_window_is_the_certificates_own() {
    let emulator = Emulator::new();
    let (ca_arn, _) = emulator
        .active_root_ca("fixture", "validity-ca.example.com", "GENERAL_PURPOSE")
        .await;
    let arn = emulator
        .request_certificate(
            "fixture",
            &json!({
                "DomainName": "validity.private.example.com",
                "CertificateAuthorityArn": &ca_arn,
            }),
        )
        .await;
    make_validation_due_now(&emulator.engine);
    let detail = describe(&emulator.router, &arn).await;
    let (status, exported) = emulator.export(&arn).await;
    assert_eq!(status, 200, "case fixture: the export succeeds, got {exported}");
    let leaf = Certificate::from_pem(
        Emulator::member("fixture", &exported, "Certificate").as_bytes(),
    )
    .expect("the exported leaf is X.509");

    for (member, instant) in [
        (
            "NotBefore",
            leaf.tbs_certificate.validity.not_before.to_system_time(),
        ),
        (
            "NotAfter",
            leaf.tbs_certificate.validity.not_after.to_system_time(),
        ),
    ] {
        let reported = detail[member]
            .as_f64()
            .unwrap_or_else(|| panic!("case {member}: expected a numeric {member}, got {detail}"));
        let expected = instant
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("an instant after the epoch")
            .as_secs() as f64;
        assert_eq!(
            reported, expected,
            "case {member}: the record reports the certificate's own {member}, got {detail}"
        );
    }
}
