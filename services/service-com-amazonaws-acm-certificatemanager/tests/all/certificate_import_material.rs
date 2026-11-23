// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `ImportCertificate` holds the MATERIAL it is handed to:
//! the `CertificateChain` body and the correspondence between `PrivateKey` and the
//! certificate's public key.
//!
//! RULES under test:
//!
//! * a `CertificateChain` is material the service stores to serve back beside the
//!   certificate, not a claim the import is checked against — so a chain is accepted
//!   whatever authority its certificates belong to and whatever they assert, is handed
//!   back by `GetCertificate` as it was sent, and never contributes a fact the record
//!   reports (`Issuer` stays the name the imported certificate's own DER carries);
//! * the only thing a chain is held to is being a PEM encoding of certificates, refused
//!   with the modelled constraint failure for supplied input under its own fixed
//!   sentence;
//! * a `PrivateKey` that is well formed but belongs to another key pair is the same
//!   class of refusal, under its own fixed sentence, and leaves the certificate stored
//!   at the ARN the request named exactly as it was;
//! * the `Certificate` member is read as the PEM certificate messages its body frames
//!   and holds exactly ONE of them, so a body framing any other number is refused with
//!   that same class under the fixed sentence naming the member's one-certificate
//!   constraint, while a body framing exactly one whose content is not a certificate is
//!   refused under the sentence naming the encoding.
//!
//! Every certificate here is built by `rcgen` — something other than the code under
//! test — and every assertion goes through the same router the emulator serves
//! production traffic on. The tables span the classes the rules generalise over: a
//! chain of an unrelated authority, of the leaf itself, of a certificate that is
//! explicitly not a CA, and of several certificates in either order; the chain defects
//! in each form (no PEM framing at all, PEM framing whose content is not a
//! certificate, a PEM of another type, a type mixed in below a certificate); and key
//! mismatches within one curve and across curves, on a
//! fresh import and on a reimport.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_import::{blob, import};
use crate::certificate_lifecycle::{
    call, describe, error_code, error_message, list_all_arns, router,
};

/// The fixed sentence each material refusal this suite exercises carries. They are the
/// only thing telling the refusals apart, so each is asserted in full.
const CHAIN_NOT_PEM: &str = "The certificate chain is not PEM-encoded or is not valid.";
const CHAIN_WRONG_PEM: &str = "Passed PEM is not a certificate chain.";
const KEY_MISMATCH: &str = "Key provided does not match the certificate.";
/// The sentence ACM answers for a `Certificate` body that does not frame exactly one
/// certificate, as recorded against the real service — the recording that fixes it is a
/// body framing NO PEM message at all, which draws this sentence and not the one below:
/// fixed text in full, naming nothing of the request.
const NOT_ONE_CERTIFICATE: &str = "The certificate field contains more than one \
                                   certificate. You can specify only one certificate in \
                                   this field.";
/// The sentence ACM answers for a `Certificate` body that frames exactly one PEM
/// certificate message whose content it cannot read as an X.509 certificate, as recorded
/// against the real service.
const CERTIFICATE_NOT_PEM: &str = "The certificate is not PEM-encoded or is not valid.";

/// What one generated certificate is built from: its subject common name, whether it
/// asserts `BasicConstraints` ca=true, its validity window as `(not_before, not_after)`
/// years, and the algorithm its key pair uses.
struct Spec<'a> {
    common_name: &'a str,
    ca: bool,
    validity: (i32, i32),
    algorithm: &'static rcgen::SignatureAlgorithm,
}

impl Default for Spec<'_> {
    fn default() -> Self {
        Self {
            common_name: "material.example.com",
            ca: false,
            // A window open across any instant a test runs at.
            validity: (2020, 2100),
            algorithm: &rcgen::PKCS_ECDSA_P256_SHA256,
        }
    }
}

/// One generated certificate: the two PEM bodies an import carries.
struct Minted {
    certificate: String,
    private_key: String,
}

/// The `CertificateParams` `spec` describes.
fn params(spec: &Spec<'_>) -> rcgen::CertificateParams {
    let mut params = rcgen::CertificateParams::new(vec![spec.common_name.to_string()])
        .expect("certificate parameters");
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(rcgen::DnType::CommonName, spec.common_name);
    params.distinguished_name = subject;
    params.not_before = rcgen::date_time_ymd(spec.validity.0, 1, 1);
    params.not_after = rcgen::date_time_ymd(spec.validity.1, 1, 1);
    params.is_ca = if spec.ca {
        rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained)
    } else {
        // `BasicConstraints` present with ca=false, so the chain member really does
        // assert that it is not a certificate authority.
        rcgen::IsCa::ExplicitNoCa
    };
    params
}

/// Generate a real self-signed certificate from `spec`, beside the key it belongs to.
fn self_signed(spec: &Spec<'_>) -> Minted {
    let key = rcgen::KeyPair::generate_for(spec.algorithm).expect("a key pair");
    let certificate = params(spec)
        .self_signed(&key)
        .expect("a self-signed certificate")
        .pem();
    Minted {
        certificate,
        private_key: key.serialize_pem(),
    }
}

/// A certificate authority a leaf can be signed by, beside the PEM of its own
/// certificate.
struct Authority {
    certificate: rcgen::Certificate,
    key: rcgen::KeyPair,
    pem: String,
    /// The authority's subject common name, which is therefore the `Issuer` ACM must
    /// report for every leaf it signs.
    common_name: String,
}

impl Authority {
    /// Generate a real certificate authority named `common_name`.
    fn new(common_name: &str) -> Self {
        let spec = Spec {
            common_name,
            ca: true,
            ..Spec::default()
        };
        let key = rcgen::KeyPair::generate_for(spec.algorithm).expect("a key pair");
        let certificate = params(&spec)
            .self_signed(&key)
            .expect("a certificate authority");
        let pem = certificate.pem();
        Self {
            certificate,
            key,
            pem,
            common_name: common_name.to_string(),
        }
    }

    /// Generate a real leaf certificate from `spec`, SIGNED by this authority — so the
    /// leaf's own DER names this authority as its issuer.
    fn sign(&self, spec: &Spec<'_>) -> Minted {
        let key = rcgen::KeyPair::generate_for(spec.algorithm).expect("a key pair");
        let certificate = params(spec)
            .signed_by(&key, &self.certificate, &self.key)
            .expect("a signed certificate")
            .pem();
        Minted {
            certificate,
            private_key: key.serialize_pem(),
        }
    }
}

/// The `ImportCertificate` request body for one generated certificate.
fn import_body(minted: &Minted) -> Value {
    json!({
        "Certificate": blob(&minted.certificate),
        "PrivateKey": blob(&minted.private_key),
    })
}

/// `GetCertificate`'s answer for `arn`, requiring success.
async fn get_certificate(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, response) = call(router, "GetCertificate", &json!({ "CertificateArn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCertificate must answer 200, got {status} {response}"
    );
    response
}

/// Every certificate ARN the store holds under the key algorithms this suite imports
/// under, so a refusal can be shown to have stored nothing. The default listing
/// reports only RSA key types, so the EC ones are named explicitly.
async fn stored_arns(router: &MultiServiceRouter) -> Vec<String> {
    list_all_arns(
        router,
        &json!({ "Includes": { "keyTypes": ["EC_prime256v1", "EC_secp384r1"] } }),
    )
    .await
}

/// Require that `body` is refused with HTTP 400 `ValidationException` and exactly
/// `message`.
async fn refused(router: &MultiServiceRouter, case: &str, body: &Value, message: &str) {
    let (status, response) = call(router, "ImportCertificate", body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the import to be refused with 400, got {status} {response}"
    );
    let code = error_code(&response);
    assert_eq!(
        code, "ValidationException",
        "case {case:?}: expected ValidationException, got {code:?} in {response}"
    );
    let actual = error_message(&response);
    assert_eq!(
        actual, message,
        "case {case:?}: expected the message {message:?}, got {actual:?}"
    );
}

/// RULE: the chain is stored and served back as sent, whatever its certificates are,
/// and contributes nothing to what the record reports — `Issuer` is the authority that
/// signed the IMPORTED certificate, read out of its own DER.
#[tokio::test]
async fn a_chain_is_stored_as_sent_and_decides_nothing() {
    let router = router();

    // Two unrelated authorities. The leaf is signed by the first, so `Issuer` must name
    // it however the chain is composed.
    let signer = Authority::new("signer.example.com");
    let stranger = Authority::new("stranger.example.com");
    let leaf = signer.sign(&Spec {
        common_name: "chained.example.com",
        ..Spec::default()
    });
    // A certificate that explicitly asserts it is NOT a certificate authority, so a
    // chain carrying it could not be a chain of trust at all.
    let not_a_ca = self_signed(&Spec {
        common_name: "end-entity.example.com",
        ..Spec::default()
    });

    // One row per chain shape the rule generalises over: the authority that really
    // signed the leaf, an authority that did not, the leaf itself, a non-CA
    // certificate, and several certificates in both orders.
    let chains: &[(&str, String)] = &[
        ("the authority that signed the leaf", signer.pem.clone()),
        ("an unrelated authority's certificate", stranger.pem.clone()),
        ("the leaf's own certificate", leaf.certificate.clone()),
        (
            "a certificate asserting it is not a CA",
            not_a_ca.certificate.clone(),
        ),
        (
            "an unrelated authority below a non-CA certificate",
            format!("{}{}", not_a_ca.certificate, stranger.pem),
        ),
        (
            "an unrelated authority above the signing one",
            format!("{}{}", signer.pem, stranger.pem),
        ),
    ];

    for (case, chain) in chains {
        let mut body = import_body(&leaf);
        body["CertificateChain"] = blob(chain);
        let arn = import(&router, case, &body).await;

        let detail = describe(&router, &arn).await;
        let status = detail["Status"].as_str();
        assert_eq!(
            status,
            Some("ISSUED"),
            "case {case:?}: an imported certificate is already issued, got {status:?} in {detail}"
        );
        let expected_issuer = signer.common_name.clone();
        let issuer = detail["Issuer"].as_str().unwrap_or_default();
        assert_eq!(
            issuer, expected_issuer,
            "case {case:?}: expected the Issuer read from the leaf's DER \
             ({expected_issuer:?}), got {issuer:?}"
        );
        let domain = detail["DomainName"].as_str();
        assert_eq!(
            domain,
            Some("chained.example.com"),
            "case {case:?}: the stored certificate must be the leaf, got {domain:?} in {detail}"
        );

        let served = get_certificate(&router, case, &arn).await;
        assert_eq!(
            served["Certificate"].as_str(),
            Some(leaf.certificate.as_str()),
            "case {case:?}: expected the leaf back as imported, got {served}"
        );
        assert_eq!(
            served["CertificateChain"].as_str(),
            Some(chain.as_str()),
            "case {case:?}: expected the chain back as sent, got {served}"
        );
    }
}

/// RULE: a chain is held to being a PEM encoding of certificates, and nothing else —
/// each failure the modelled constraint failure for supplied input, under its own fixed
/// sentence, with nothing stored.
#[tokio::test]
async fn a_chain_that_is_not_servable_is_refused() {
    let router = router();

    let leaf = self_signed(&Spec {
        common_name: "refused-chain.example.com",
        ..Spec::default()
    });
    let valid_ca = Authority::new("valid-ca.example.com");

    // One row per form the held-to property can fail in: no PEM framing at all, framing
    // whose content is not base64, framing whose content is not a certificate, and a
    // well-formed PEM of another type alone and mixed in below a certificate.
    let cases: &[(&str, String, &str)] = &[
        ("text that frames no PEM message", "not a chain".to_string(), CHAIN_NOT_PEM),
        (
            "a CERTIFICATE block whose content is not base64",
            "-----BEGIN CERTIFICATE-----\nnot-base64!!!\n-----END CERTIFICATE-----\n".to_string(),
            CHAIN_NOT_PEM,
        ),
        (
            "a CERTIFICATE block whose content is not a certificate",
            "-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n".to_string(),
            CHAIN_NOT_PEM,
        ),
        (
            "a PEM private key",
            leaf.private_key.clone(),
            CHAIN_WRONG_PEM,
        ),
        (
            "a PEM private key below a certificate",
            format!("{}{}", valid_ca.pem, leaf.private_key),
            CHAIN_WRONG_PEM,
        ),
    ];

    for (case, chain, message) in cases {
        let mut body = import_body(&leaf);
        body["CertificateChain"] = blob(chain);
        refused(&router, case, &body, message).await;
    }

    let listed = stored_arns(&router).await;
    assert!(
        listed.is_empty(),
        "case {:?}: a refused import must store nothing, got {listed:?}",
        "every chain refusal above"
    );
}

/// RULE: a `PrivateKey` that does not belong to the certificate's public key is the
/// same class of refusal under its own fixed sentence — on a fresh import and on a
/// reimport, where the certificate already stored at the named ARN is left byte for
/// byte as it was.
#[tokio::test]
async fn a_key_that_is_not_the_certificates_is_refused() {
    let router = router();

    // The certificate the reimports target, imported with the key that really does
    // belong to it: the positive control.
    let stored = self_signed(&Spec {
        common_name: "rotating.example.com",
        ..Spec::default()
    });
    let arn = import(&router, "the certificate to replace", &import_body(&stored)).await;
    let before = get_certificate(&router, "the certificate to replace", &arn).await;
    assert_eq!(
        before["Certificate"].as_str(),
        Some(stored.certificate.as_str()),
        "case {:?}: expected the imported body back, got {before}",
        "the certificate to replace"
    );

    // One row per key-mismatch class: another key pair on the same curve, and a key
    // pair of a different curve, against certificates of each curve.
    let p256 = self_signed(&Spec {
        common_name: "rotating.example.com",
        ..Spec::default()
    });
    let p384 = self_signed(&Spec {
        common_name: "rotating.example.com",
        algorithm: &rcgen::PKCS_ECDSA_P384_SHA384,
        ..Spec::default()
    });
    let other_p256 = self_signed(&Spec::default());
    let other_p384 = self_signed(&Spec {
        algorithm: &rcgen::PKCS_ECDSA_P384_SHA384,
        ..Spec::default()
    });

    let cases: &[(&str, &Minted, &Minted)] = &[
        (
            "a P-256 certificate with another P-256 key pair's key",
            &p256,
            &other_p256,
        ),
        (
            "a P-256 certificate with a P-384 key",
            &p256,
            &other_p384,
        ),
        (
            "a P-384 certificate with another P-384 key pair's key",
            &p384,
            &other_p384,
        ),
        (
            "a P-384 certificate with a P-256 key",
            &p384,
            &other_p256,
        ),
    ];

    for (case, certificate, key) in cases {
        // A reimport onto the stored ARN, and a fresh import carrying no ARN: the
        // refusal belongs to the material, not to which of the two the request is.
        let fresh = json!({
            "Certificate": blob(&certificate.certificate),
            "PrivateKey": blob(&key.private_key),
        });
        refused(&router, case, &fresh, KEY_MISMATCH).await;

        let mut reimport = fresh.clone();
        reimport["CertificateArn"] = json!(arn);
        let reimport_case = format!("{case}, onto an existing ARN");
        refused(&router, &reimport_case, &reimport, KEY_MISMATCH).await;

        // The refused reimport changed nothing: the ARN still answers the material it
        // was imported with.
        let after = get_certificate(&router, &reimport_case, &arn).await;
        assert_eq!(
            after, before,
            "case {reimport_case:?}: a refused reimport must leave the stored material \
             byte for byte as it was, got {after}"
        );
    }

    // Nothing above was stored beside the control.
    let listed = stored_arns(&router).await;
    assert_eq!(
        listed,
        vec![arn.clone()],
        "case {:?}: only the control import may be stored, got {listed:?}",
        "every key refusal above"
    );
}

/// RULE: the `Certificate` member is read as the PEM certificate messages its body
/// frames and holds exactly ONE of them — the certificate being imported — so a body
/// framing any other number, more or none, is the modelled constraint failure for
/// supplied input under the fixed sentence naming that constraint, whichever
/// certificates the body carries, in whatever order, however many, whether or not each
/// decodes, and whatever text separates the PEM blocks. A body framing exactly one
/// certificate message is then DECODED, and one whose content is not a certificate is
/// refused under the sentence naming the encoding instead — so the first sentence is
/// reported for the count and the second for the content. The certificates above the
/// leaf belong to `CertificateChain`, where the same material is accepted and served
/// back.
#[tokio::test]
async fn the_certificate_member_carries_one_certificate() {
    let router = router();

    let signer = Authority::new("one-only-ca.example.com");
    let leaf = signer.sign(&Spec {
        common_name: "one-only.example.com",
        ..Spec::default()
    });
    let stranger = Authority::new("one-only-stranger.example.com");
    let other_leaf = self_signed(&Spec {
        common_name: "second-leaf.example.com",
        ..Spec::default()
    });
    // A well-formed CERTIFICATE block whose content is not a certificate at all, so a
    // body carrying two of them is more than one certificate AND undecodable.
    let undecodable = "-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n";

    // Rows differ along how many certificates the field carries (none, two, three),
    // which ones they are, their order, whether each decodes, what separates the PEM
    // blocks, and — for the bodies framing none — whether the body is text at all. The
    // first row is the shape the recorded ACM traffic fixes: a body framing no PEM
    // message at all (the 21 bytes `not-a-pem-certificate`) draws this sentence and not
    // the encoding one below.
    let refusals: &[(&str, Value)] = &[
        (
            "text framing no PEM message at all",
            blob("not-a-pem-certificate"),
        ),
        (
            "a boundary that does not open its own line",
            blob(&format!("leading text {}", leaf.certificate)),
        ),
        (
            "a single character",
            blob("x"),
        ),
        (
            "a well-formed PEM of another type",
            blob(&leaf.private_key),
        ),
        (
            "a CERTIFICATE REQUEST block",
            blob("-----BEGIN CERTIFICATE REQUEST-----\nZm9v\n-----END CERTIFICATE REQUEST-----\n"),
        ),
        (
            "bytes that are not text",
            json!(aws_smithy_types::base64::encode([0xff_u8, 0xfe, 0x00, 0x80])),
        ),
        (
            "the leaf with its own signing authority below it",
            blob(&format!("{}{}", leaf.certificate, signer.pem)),
        ),
        (
            "the signing authority above the leaf",
            blob(&format!("{}{}", signer.pem, leaf.certificate)),
        ),
        (
            "the same certificate twice",
            blob(&format!("{}{}", leaf.certificate, leaf.certificate)),
        ),
        (
            "the leaf with an unrelated authority",
            blob(&format!("{}{}", leaf.certificate, stranger.pem)),
        ),
        (
            "the leaf with another end-entity certificate",
            blob(&format!("{}{}", leaf.certificate, other_leaf.certificate)),
        ),
        (
            "three certificates",
            blob(&format!(
                "{}{}{}",
                leaf.certificate, signer.pem, stranger.pem
            )),
        ),
        (
            "two certificates separated by a blank line",
            blob(&format!("{}\n{}", leaf.certificate, signer.pem)),
        ),
        (
            "two certificates with CRLF line endings",
            blob(&format!("{}{}", leaf.certificate, signer.pem).replace('\n', "\r\n")),
        ),
        (
            "the leaf below a CERTIFICATE block framing no certificate",
            blob(&format!("{undecodable}{}", leaf.certificate)),
        ),
        (
            "two CERTIFICATE blocks framing no certificate",
            blob(&format!("{undecodable}{undecodable}")),
        ),
    ];

    for (case, body) in refusals {
        let mut request = import_body(&leaf);
        request["Certificate"] = body.clone();
        refused(&router, case, &request, NOT_ONE_CERTIFICATE).await;
    }

    // Exactly one certificate message is a body this member holds, so what it frames is
    // then decoded and the refusal names the encoding instead.
    let one_message: &[(&str, String)] = &[
        (
            "one CERTIFICATE block framing no certificate",
            undecodable.to_string(),
        ),
        (
            "one CERTIFICATE block whose base64 is damaged",
            leaf.certificate.replacen('M', "!", 1),
        ),
        (
            "the certificate with a PEM private key below it",
            format!("{}{}", leaf.certificate, leaf.private_key),
        ),
    ];

    for (case, body) in one_message {
        let mut request = import_body(&leaf);
        request["Certificate"] = blob(body);
        refused(&router, case, &request, CERTIFICATE_NOT_PEM).await;
    }

    let listed = stored_arns(&router).await;
    assert!(
        listed.is_empty(),
        "case {:?}: a refused import must store nothing, got {listed:?}",
        "every Certificate-member refusal above"
    );

    // The same material in the two members it belongs in: the leaf alone as the
    // certificate, its authority as the chain — accepted, and served back as sent.
    let case = "the leaf alone, with its authority as the chain";
    let mut request = import_body(&leaf);
    request["CertificateChain"] = blob(&signer.pem);
    let arn = import(&router, case, &request).await;
    let served = get_certificate(&router, case, &arn).await;
    assert_eq!(
        served["Certificate"].as_str(),
        Some(leaf.certificate.as_str()),
        "case {case:?}: expected the one imported certificate back, got {served}"
    );
    assert_eq!(
        served["CertificateChain"].as_str(),
        Some(signer.pem.as_str()),
        "case {case:?}: expected the chain back as sent, got {served}"
    );
}
