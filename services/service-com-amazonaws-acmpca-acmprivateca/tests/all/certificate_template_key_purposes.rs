// SPDX-License-Identifier: CC0-1.0

//! The key purposes a certificate template defines, read off the certificate the service
//! issues under it.
//!
//! THE RULE: a `TemplateArn` "Specifies a custom configuration template to use when issuing a
//! certificate" (`IssueCertificateRequest$TemplateArn`), and a template is a named set of
//! X.509 extensions: every published definition states the `basicConstraints` of the
//! certificate it defines and the `keyUsage` of the key it certifies, and the kinds that
//! certify a key for a NAMED purpose state an `extendedKeyUsage` as well. So the certificate
//! issued under a template carries exactly the extensions that template defines — a CA
//! certificate the usages an authority needs (certificate signing, CRL signing, digital
//! signature) and no key purpose, an end-entity certificate the usages and the purposes its
//! kind is for — while a blank kind, whose content an `ApiPassthrough` fills in, and a kind
//! outside the published catalog define none.
//!
//! The rows span the published `TemplateArn` catalog ("For a list of TemplateArn values
//! supported by Amazon Web Services Private CA, see Understanding Certificate Templates",
//! `IssueCertificateRequest$TemplateArn`): the CA kinds, each end-entity kind, a passthrough
//! variant, a blank variant, a blank variant that names critical basic constraints, a request
//! that names no template at all, and a kind this service does not know.

use std::time::{Duration, Instant};

use serde_json::json;
use x509_cert::der::Decode;
use x509_cert::ext::pkix::{ExtendedKeyUsage, KeyUsage, KeyUsages};
use x509_cert::ext::Extension;
use x509_cert::Certificate;

use crate::private_ca_lifecycle::{
    active_ca, blob, certificate_of, csr_pem, string, Emulator,
};

/// The `keyUsage` extension, RFC 5280 section 4.2.1.3.
const KEY_USAGE_OID: &str = "2.5.29.15";

/// The `extendedKeyUsage` extension, RFC 5280 section 4.2.1.12.
const EXTENDED_KEY_USAGE_OID: &str = "2.5.29.37";

/// The key purposes the published definitions name, by the object identifiers RFC 5280
/// section 4.2.1.12 assigns them.
const TLS_SERVER_AUTHENTICATION: &str = "1.3.6.1.5.5.7.3.1";
const TLS_CLIENT_AUTHENTICATION: &str = "1.3.6.1.5.5.7.3.2";
const CODE_SIGNING: &str = "1.3.6.1.5.5.7.3.3";
const OCSP_SIGNING: &str = "1.3.6.1.5.5.7.3.9";

/// Every named bit of the `keyUsage` extension, in the order RFC 5280 section 4.2.1.3 lists
/// them, so a row names the bits it expects rather than a bit string.
const KEY_USAGE_BITS: &[(&str, KeyUsages)] = &[
    ("digitalSignature", KeyUsages::DigitalSignature),
    ("nonRepudiation", KeyUsages::NonRepudiation),
    ("keyEncipherment", KeyUsages::KeyEncipherment),
    ("dataEncipherment", KeyUsages::DataEncipherment),
    ("keyAgreement", KeyUsages::KeyAgreement),
    ("keyCertSign", KeyUsages::KeyCertSign),
    ("cRLSign", KeyUsages::CRLSign),
    ("encipherOnly", KeyUsages::EncipherOnly),
    ("decipherOnly", KeyUsages::DecipherOnly),
];

/// How long a poll for a due transition may take before the suite gives up. Generous: it
/// bounds a wait for a CONDITION, not a measurement of how long the service takes.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Poll `GetCertificate` until the issuance has completed, and answer the certificate it
/// serves.
async fn served_certificate(
    emulator: &Emulator,
    case: &str,
    ca_arn: &str,
    certificate_arn: &str,
) -> Certificate {
    let body = json!({
        "CertificateAuthorityArn": ca_arn,
        "CertificateArn": certificate_arn,
    });
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, parsed) = emulator.call("GetCertificate", &body).await;
        if status == 200 {
            return certificate_of(&string(&parsed, "Certificate", case), case);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {certificate_arn} to be served, got {status} {parsed:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The extension of `certificate` whose object identifier is `oid`, when it carries one.
fn extension(certificate: &Certificate, oid: &str) -> Option<Extension> {
    certificate
        .tbs_certificate
        .extensions
        .as_ref()?
        .iter()
        .find(|extension| extension.extn_id.to_string() == oid)
        .cloned()
}

/// The named `keyUsage` bits `certificate` asserts, in RFC 5280's order; none when it carries
/// no such extension.
fn asserted_key_usage(certificate: &Certificate, case: &str) -> Vec<&'static str> {
    let Some(carried) = extension(certificate, KEY_USAGE_OID) else {
        return Vec::new();
    };
    assert!(
        carried.critical,
        "case {case:?}: key usage is critical, got {carried:?}"
    );
    let usage = KeyUsage::from_der(carried.extn_value.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: expected a DER key usage: {error}"));
    KEY_USAGE_BITS
        .iter()
        .filter(|(_, bit)| usage.0.contains(*bit))
        .map(|(name, _)| *name)
        .collect()
}

/// The key purposes `certificate` names, as their dotted object identifiers; none when it
/// carries no `extendedKeyUsage`.
fn named_key_purposes(certificate: &Certificate, case: &str) -> Vec<String> {
    let Some(carried) = extension(certificate, EXTENDED_KEY_USAGE_OID) else {
        return Vec::new();
    };
    // Not critical, so a relying party that does not recognise a purpose may still accept
    // the certificate.
    assert!(
        !carried.critical,
        "case {case:?}: extended key usage is not critical, got {carried:?}"
    );
    let purposes = ExtendedKeyUsage::from_der(carried.extn_value.as_bytes()).unwrap_or_else(
        |error| panic!("case {case:?}: expected a DER extended key usage: {error}"),
    );
    purposes
        .0
        .iter()
        .map(::std::string::ToString::to_string)
        .collect()
}

/// One row: the template a request names, the `keyUsage` bits the certificate must assert and
/// the key purposes it must name.
type Row<'a> = (&'a str, Option<&'a str>, &'a [&'a str], &'a [&'a str]);

/// THE RULE: the certificate issued under a template carries the key usage and the key
/// purposes that template defines.
#[tokio::test]
async fn an_issued_certificate_carries_the_key_purposes_its_template_defines() {
    let emulator = Emulator::new();
    let (ca, _certificate) = active_ca(&emulator, "key-purpose-root").await;

    let rows: Vec<Row<'static>> = vec![
        (
            "no template at all, which is the end-entity default",
            None,
            &["digitalSignature", "keyEncipherment"],
            &[TLS_SERVER_AUTHENTICATION, TLS_CLIENT_AUTHENTICATION],
        ),
        (
            "the end-entity default named explicitly",
            Some("arn:aws:acm-pca:::template/EndEntityCertificate/V1"),
            &["digitalSignature", "keyEncipherment"],
            &[TLS_SERVER_AUTHENTICATION, TLS_CLIENT_AUTHENTICATION],
        ),
        (
            "the client-authentication kind",
            Some("arn:aws:acm-pca:::template/EndEntityClientAuthCertificate/V1"),
            &["digitalSignature", "keyEncipherment"],
            &[TLS_CLIENT_AUTHENTICATION],
        ),
        (
            "the server-authentication kind, passing the API through",
            Some("arn:aws:acm-pca:::template/EndEntityServerAuthCertificate_APIPassthrough/V1"),
            &["digitalSignature", "keyEncipherment"],
            &[TLS_SERVER_AUTHENTICATION],
        ),
        (
            "the code-signing kind",
            Some("arn:aws:acm-pca:::template/CodeSigningCertificate/V1"),
            &["digitalSignature"],
            &[CODE_SIGNING],
        ),
        (
            "the OCSP-signing kind",
            Some("arn:aws:acm-pca:::template/OCSPSigningCertificate/V1"),
            &["digitalSignature"],
            &[OCSP_SIGNING],
        ),
        (
            "the root CA kind",
            Some("arn:aws:acm-pca:::template/RootCACertificate/V1"),
            &["digitalSignature", "keyCertSign", "cRLSign"],
            &[],
        ),
        (
            "a subordinate CA of depth zero",
            Some("arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen0/V1"),
            &["digitalSignature", "keyCertSign", "cRLSign"],
            &[],
        ),
        (
            "a subordinate CA of depth two, passing the CSR through",
            Some(
                "arn:aws:acm-pca:::template/\
                 SubordinateCACertificate_PathLen2_APICSRPassthrough/V1",
            ),
            &["digitalSignature", "keyCertSign", "cRLSign"],
            &[],
        ),
        // A blank kind defines nothing of its own: an `ApiPassthrough` is what fills such a
        // certificate in.
        (
            "a blank end-entity kind",
            Some("arn:aws:acm-pca:::template/BlankEndEntityCertificate_APIPassthrough/V1"),
            &[],
            &[],
        ),
        // ... and one whose name says it defines critical basic constraints defines that one
        // extension and no other.
        (
            "a blank subordinate CA kind that names critical basic constraints",
            Some(
                "arn:aws:acm-pca:::template/\
                 BlankSubordinateCACertificate_PathLen0_CriticalBasicConstraints_APIPassthrough/V1",
            ),
            &[],
            &[],
        ),
        (
            "a template kind outside the published ones",
            Some("arn:aws:acm-pca:::template/NoSuchCertificateKind/V1"),
            &[],
            &[],
        ),
    ];

    // Issue every row first, then read each certificate back.
    let mut issued: Vec<(&Row<'static>, String)> = Vec::new();
    for row in &rows {
        let (case, template, _, _) = row;
        let mut body = json!({
            "CertificateAuthorityArn": ca,
            "Csr": blob(&csr_pem(&format!("key-purpose-leaf-{}", issued.len()))),
            "SigningAlgorithm": "SHA256WITHRSA",
            "Validity": {"Type": "DAYS", "Value": 30},
        });
        if let Some(template) = template {
            body["TemplateArn"] = json!(template);
        }
        let answered = emulator.ok(case, "IssueCertificate", &body).await;
        issued.push((row, string(&answered, "CertificateArn", case)));
    }

    for (row, certificate_arn) in &issued {
        let (case, _, usage, purposes) = row;
        let certificate = served_certificate(&emulator, case, &ca, certificate_arn).await;
        let asserted = asserted_key_usage(&certificate, case);
        assert_eq!(
            asserted, *usage,
            "case {case:?}: expected the key usage {usage:?}, got {asserted:?}"
        );
        let named = named_key_purposes(&certificate, case);
        let expected: Vec<String> = purposes.iter().map(|oid| (*oid).to_string()).collect();
        assert_eq!(
            named, expected,
            "case {case:?}: expected the key purposes {expected:?}, got {named:?}"
        );
    }
}
