// SPDX-License-Identifier: CC0-1.0

//! The key material a private CA is activated with: the key it publishes, and every key it
//! refuses.
//!
//! The rule under test: a private CA signs with the key pair IT holds, so the certification
//! request it publishes carries a complete `SubjectPublicKeyInfo` for that pair — one any
//! X.509 tooling reads as a key of the configured `KeyAlgorithm` and verifies the request's
//! own signature under — and the certificate the CA takes must be over exactly that key. A
//! certificate whose subject IS the CA's but whose `SubjectPublicKeyInfo` is any other key
//! (another key of the same algorithm, a key of another size, or a key of another algorithm
//! family) is refused with `CertificateMismatchException` and the sentence about the subject
//! public key info field, nothing is installed, and the SAME CA still activates with the
//! certificate its own request asks for — which is what says the refusal was about the key
//! rather than a blanket refusal.
//!
//! The rows span the CA's own `KeyAlgorithm` (two RSA sizes and a NIST curve, not only the
//! `RSA_2048` the reports cite) crossed with how the presented key diverges (bits, size,
//! algorithm family), and then the subject shapes a real client presents a genuinely
//! self-signed CA certificate under: the key material is what diverges in every row.
//!
//! The foreign keys and the self-signed certificates are minted here, outside the code under
//! test, so the bytes the service judges are not ones this crate wrote.

use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{
    EcdsaKeyPair, EcdsaSigningAlgorithm, KeyPair as _, RsaKeyPair, UnparsedPublicKey,
    VerificationAlgorithm, ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_ASN1_SIGNING,
    ECDSA_P384_SHA384_ASN1_SIGNING, RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_2048_8192_SHA384,
};
use serde_json::{json, Value};
use x509_cert::der::{Decode, Encode};
use x509_cert::spki::SubjectPublicKeyInfoOwned;

use crate::private_ca_lifecycle::{
    blob, ca_certificate_for, certificate_of, certificate_over, request_of, string,
    wait_for_the_clock, Emulator,
};
use crate::resource_policy::error_message;

/// What Amazon Web Services Private CA answers when the presented certificate is for the
/// CA's own subject and is over another key: fixed text in full, interpolating nothing from
/// the request or from state.
///
/// Written out here rather than read off the service's own constant, so a change to either
/// has to be made in both.
const PUBLIC_KEY_MISMATCH_MESSAGE: &str =
    "The certificate subject public key info field contains a different public key or key \
     algorithm than the CA certificate signing request.";

/// One CA configuration a row drives, with the algorithm its published key verifies a
/// signature under.
struct CaRow {
    /// The case name every assertion of this row names.
    case: &'static str,
    /// The `KeyAlgorithm` the CA is created with.
    key_algorithm: &'static str,
    /// A `SigningAlgorithm` of that key's family.
    signing_algorithm: &'static str,
    /// The algorithm a signature the CA's key made verifies under.
    verification: &'static dyn VerificationAlgorithm,
}

/// The CA configurations the rows below drive: both ends of the RSA sizes this service
/// mints, and a NIST curve, so the rule is asserted across key algorithm FAMILIES rather
/// than for one member.
fn ca_rows() -> Vec<CaRow> {
    vec![
        CaRow {
            case: "an RSA_2048 CA",
            key_algorithm: "RSA_2048",
            signing_algorithm: "SHA256WITHRSA",
            verification: &RSA_PKCS1_2048_8192_SHA256,
        },
        CaRow {
            case: "an RSA_3072 CA",
            key_algorithm: "RSA_3072",
            signing_algorithm: "SHA384WITHRSA",
            verification: &RSA_PKCS1_2048_8192_SHA384,
        },
        CaRow {
            case: "an EC_prime256v1 CA",
            key_algorithm: "EC_prime256v1",
            signing_algorithm: "SHA256WITHECDSA",
            verification: &ECDSA_P256_SHA256_ASN1,
        },
    ]
}

/// How a presented key can diverge from the one a CA published, for a CA of
/// `key_algorithm`: another key of the same algorithm, a key of another size of the same
/// family, and a key of another family altogether.
///
/// The members named are `KeyAlgorithm`'s own
/// (`smithy_query(query="shape",shape="KeyAlgorithm")`), so every presented key is one a CA
/// of this service could itself have been created with — what makes it foreign is only that
/// this CA did not publish it.
fn foreign_keys(key_algorithm: &str) -> Vec<(&'static str, SubjectPublicKeyInfoOwned)> {
    match key_algorithm {
        "RSA_2048" => vec![
            ("another RSA_2048 key", rsa_public_key(KeySize::Rsa2048)),
            ("an RSA_3072 key", rsa_public_key(KeySize::Rsa3072)),
            (
                "an EC_prime256v1 key",
                ecdsa_public_key(&ECDSA_P256_SHA256_ASN1_SIGNING),
            ),
        ],
        "RSA_3072" => vec![
            ("another RSA_3072 key", rsa_public_key(KeySize::Rsa3072)),
            ("an RSA_2048 key", rsa_public_key(KeySize::Rsa2048)),
            (
                "an EC_secp384r1 key",
                ecdsa_public_key(&ECDSA_P384_SHA384_ASN1_SIGNING),
            ),
        ],
        _ => vec![
            (
                "another EC_prime256v1 key",
                ecdsa_public_key(&ECDSA_P256_SHA256_ASN1_SIGNING),
            ),
            (
                "an EC_secp384r1 key",
                ecdsa_public_key(&ECDSA_P384_SHA384_ASN1_SIGNING),
            ),
            ("an RSA_2048 key", rsa_public_key(KeySize::Rsa2048)),
        ],
    }
}

/// A fresh RSA public key of `size`, as the `SubjectPublicKeyInfo` a certificate carries.
fn rsa_public_key(size: KeySize) -> SubjectPublicKeyInfoOwned {
    let key = RsaKeyPair::generate(size).expect("an RSA key pair");
    let der: PublicKeyX509Der = AsDer::as_der(key.public_key()).expect("the public key encodes");
    public_key_info(der.as_ref())
}

/// A fresh EC public key on the curve `algorithm` names, as the `SubjectPublicKeyInfo` a
/// certificate carries.
fn ecdsa_public_key(algorithm: &'static EcdsaSigningAlgorithm) -> SubjectPublicKeyInfoOwned {
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(algorithm, &SystemRandom::new()).expect("an EC key pair");
    let key = EcdsaKeyPair::from_pkcs8(algorithm, pkcs8.as_ref()).expect("the key pair loads");
    let der: PublicKeyX509Der = AsDer::as_der(key.public_key()).expect("the public key encodes");
    public_key_info(der.as_ref())
}

/// A DER `SubjectPublicKeyInfo`, decoded.
fn public_key_info(der: &[u8]) -> SubjectPublicKeyInfoOwned {
    SubjectPublicKeyInfoOwned::from_der(der).expect("a SubjectPublicKeyInfo")
}

/// A `CreateCertificateAuthority` body for `row`, under a subject carrying the three X.500
/// attributes a client script usually configures.
fn create_request(row: &CaRow, common_name: &str) -> Value {
    json!({
        "CertificateAuthorityConfiguration": {
            "KeyAlgorithm": row.key_algorithm,
            "SigningAlgorithm": row.signing_algorithm,
            "Subject": {
                "CommonName": common_name,
                "Country": "US",
                "Organization": "Example Inc",
            },
        },
        "CertificateAuthorityType": "ROOT",
    })
}

/// An `ImportCertificateAuthorityCertificate` body.
fn import(arn: &str, certificate: &str) -> Value {
    json!({"CertificateAuthorityArn": arn, "Certificate": blob(certificate)})
}

/// The `Status` a CA reports.
async fn status(emulator: &Emulator, case: &str, arn: &str) -> String {
    let described = emulator
        .ok(
            case,
            "DescribeCertificateAuthority",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await;
    string(&described["CertificateAuthority"], "Status", case)
}

/// Every key that is not the one the CA published is refused, whatever the CA's own key
/// algorithm and however the presented key diverges — and the CA then activates with the
/// certificate its own request asks for.
#[tokio::test]
async fn an_import_over_any_key_but_the_published_one_is_refused_as_a_key_mismatch() {
    let emulator = Emulator::new();
    let rows = ca_rows();

    // Create every CA first and wait for the clock ONCE: a CA publishes its certification
    // request when its creation settles.
    let mut authorities: Vec<String> = Vec::with_capacity(rows.len());
    for row in &rows {
        let created = emulator
            .ok(
                row.case,
                "CreateCertificateAuthority",
                &create_request(row, &format!("identity-{}", row.key_algorithm)),
            )
            .await;
        authorities.push(string(&created, "CertificateAuthorityArn", row.case));
    }
    wait_for_the_clock();

    for (row, arn) in rows.iter().zip(&authorities) {
        let case = row.case;
        let csr = string(
            &emulator
                .ok(
                    case,
                    "GetCertificateAuthorityCsr",
                    &json!({"CertificateAuthorityArn": arn}),
                )
                .await,
            "Csr",
            case,
        );
        let request = request_of(&csr, case);
        let published = request.info.public_key.clone();

        // The published key is a COMPLETE key of the configured algorithm, not a structure
        // of the right shape: it verifies the signature the CA made over the request it
        // sits in, which nothing but the matching private half can produce.
        let signed = request
            .info
            .to_der()
            .unwrap_or_else(|error| panic!("case {case:?}: the request body re-encodes: {error}"));
        UnparsedPublicKey::new(row.verification, published.subject_public_key.raw_bytes())
            .verify(&signed, request.signature.raw_bytes())
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the published request must be signed by the key it \
                     publishes, got {error:?}"
                )
            });

        for (kind, foreign) in foreign_keys(row.key_algorithm) {
            let case = format!("{case} presented its own subject over {kind}");
            assert_ne!(
                foreign.to_der().ok(),
                published.to_der().ok(),
                "case {case:?}: the presented key must differ from the published one"
            );
            // Only the key diverges: the subject is the one the CA's own request carries.
            let certificate =
                certificate_over(&request.info.subject, &foreign, &request.algorithm);
            let answer = emulator
                .refused(
                    &case,
                    "ImportCertificateAuthorityCertificate",
                    &import(arn, &certificate),
                    "CertificateMismatchException",
                )
                .await;
            let message = error_message(&answer, &case);
            assert_eq!(
                message, PUBLIC_KEY_MISMATCH_MESSAGE,
                "case {case:?}: expected the subject-public-key-info sentence, got {message:?}"
            );

            // Nothing was installed: the CA still waits for its certificate and still
            // serves none.
            let reported = status(&emulator, &case, arn).await;
            assert_eq!(
                reported, "PENDING_CERTIFICATE",
                "case {case:?}: a refused import must leave the CA waiting, got {reported:?}"
            );
            emulator
                .refused(
                    &case,
                    "GetCertificateAuthorityCertificate",
                    &json!({"CertificateAuthorityArn": arn}),
                    "InvalidStateException",
                )
                .await;
        }

        // The certificate of the CA's OWN request is taken, which is what says every
        // refusal above was about the key material rather than a blanket refusal.
        let case = format!("{case} presented the certificate of its own request");
        emulator
            .ok(
                &case,
                "ImportCertificateAuthorityCertificate",
                &import(arn, &ca_certificate_for(&csr)),
            )
            .await;
        let reported = status(&emulator, &case, arn).await;
        assert_eq!(
            reported, "ACTIVE",
            "case {case:?}: the certificate of the CA's own request activates it, got \
             {reported:?}"
        );
        let served = emulator
            .ok(
                &case,
                "GetCertificateAuthorityCertificate",
                &json!({"CertificateAuthorityArn": arn}),
            )
            .await;
        let installed = certificate_of(&string(&served, "Certificate", &case), &case);
        assert_eq!(
            installed.tbs_certificate.subject_public_key_info, published,
            "case {case:?}: the installed certificate must carry the key the CA published"
        );
    }
}

/// A genuinely self-signed CA certificate — a real signature its own key made, with basic
/// constraints marked critical — carrying the CA's exact subject and an unrelated key is
/// refused over the key, under every subject shape a client configures.
#[tokio::test]
async fn a_self_signed_certificate_for_the_cas_subject_over_another_key_is_refused() {
    let emulator = Emulator::new();

    // The subject shapes: one attribute, and the three-attribute name a client script
    // configures. The CA of each row is created with that subject, and the foreign
    // certificate is built under the same attributes, so the key is all that diverges.
    let subjects: Vec<(&str, Value, rcgen::DistinguishedName)> = vec![
        (
            "one attribute",
            json!({"CommonName": "self-signed-one"}),
            distinguished_name("self-signed-one", None, None),
        ),
        (
            "three attributes",
            json!({
                "CommonName": "self-signed-three",
                "Country": "US",
                "Organization": "Example Inc",
            }),
            distinguished_name("self-signed-three", Some("US"), Some("Example Inc")),
        ),
    ];

    let mut authorities: Vec<String> = Vec::with_capacity(subjects.len());
    for (case, subject, _) in &subjects {
        let body = json!({
            "CertificateAuthorityConfiguration": {
                "KeyAlgorithm": "RSA_2048",
                "SigningAlgorithm": "SHA256WITHRSA",
                "Subject": subject,
            },
            "CertificateAuthorityType": "ROOT",
        });
        let created = emulator
            .ok(case, "CreateCertificateAuthority", &body)
            .await;
        authorities.push(string(&created, "CertificateAuthorityArn", case));
    }
    wait_for_the_clock();

    for ((case, _, name), arn) in subjects.iter().zip(&authorities) {
        let csr = string(
            &emulator
                .ok(
                    case,
                    "GetCertificateAuthorityCsr",
                    &json!({"CertificateAuthorityArn": arn}),
                )
                .await,
            "Csr",
            case,
        );
        let answer = emulator
            .refused(
                case,
                "ImportCertificateAuthorityCertificate",
                &import(arn, &self_signed_authority(name)),
                "CertificateMismatchException",
            )
            .await;
        let message = error_message(&answer, case);
        assert_eq!(
            message, PUBLIC_KEY_MISMATCH_MESSAGE,
            "case {case:?}: expected the subject-public-key-info sentence, got {message:?}"
        );
        let reported = status(&emulator, case, arn).await;
        assert_eq!(
            reported, "PENDING_CERTIFICATE",
            "case {case:?}: a refused import must leave the CA waiting, got {reported:?}"
        );
        emulator
            .refused(
                case,
                "GetCertificateAuthorityCertificate",
                &json!({"CertificateAuthorityArn": arn}),
                "InvalidStateException",
            )
            .await;

        // And the same CA activates with the certificate of its own request.
        emulator
            .ok(
                case,
                "ImportCertificateAuthorityCertificate",
                &import(arn, &ca_certificate_for(&csr)),
            )
            .await;
        let reported = status(&emulator, case, arn).await;
        assert_eq!(
            reported, "ACTIVE",
            "case {case:?}: the certificate of the CA's own request activates it, got \
             {reported:?}"
        );
    }
}

/// An X.500 name of the attributes a CA's configured `ASN1Subject` carries: the common name,
/// and the country and organization when the configuration names them (RFC 4519 sections
/// 2.2, 2.4 and 2.19).
fn distinguished_name(
    common_name: &str,
    country: Option<&str>,
    organization: Option<&str>,
) -> rcgen::DistinguishedName {
    let mut name = rcgen::DistinguishedName::new();
    name.push(rcgen::DnType::CommonName, common_name);
    if let Some(country) = country {
        name.push(rcgen::DnType::CountryName, country);
    }
    if let Some(organization) = organization {
        name.push(rcgen::DnType::OrganizationName, organization);
    }
    name
}

/// A real, self-signed CA certificate for `name`, over a key pair minted for it alone and
/// signed with that pair's private half, with basic constraints marked critical — the
/// certificate a client builds when it signs the wrong request, carrying a key the service
/// never published.
fn self_signed_authority(name: &rcgen::DistinguishedName) -> String {
    let key = rcgen::KeyPair::generate().expect("a key pair");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("parameters");
    params.distinguished_name = name.clone();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .self_signed(&key)
        .expect("a self-signed certificate")
        .pem()
}
