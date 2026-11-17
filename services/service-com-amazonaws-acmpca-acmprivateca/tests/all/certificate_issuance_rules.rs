// SPDX-License-Identifier: CC0-1.0

//! The rules an issuance follows, and the sentences the certificate path refuses with.
//!
//! The rules under test, none of them about one particular request:
//!
//! * a certificate's validity period must end no later than the issuing CA's own
//!   certificate does — "The end of the validity period configured on a certificate must
//!   not exceed the limit set on its parents in the CA hierarchy."
//!   (`IssueCertificateRequest$Validity`) — and the limit is the `NotAfter` of the
//!   certificate the CA was activated with, whichever way the request expresses its own;
//! * `IssueCertificate` is `@idempotent`, so a repeat of a request a CA has already
//!   answered is answered with the certificate that call minted, while a request differing
//!   in any member that decides the certificate's content gets one of its own;
//! * the extensions an issued certificate carries are the ones its `TemplateArn` defines —
//!   `basicConstraints`, critical, with the template's CA flag and the `pathLenConstraint`
//!   its `PathLenN` portion publishes — plus the subject alternative name the certification
//!   request asks for;
//! * an `ApiPassthrough` is applied only under a template variant whose name selects it, and
//!   where it is, the subject and extensions it supplies take priority over the certification
//!   request's;
//! * every certificate the CA writes publishes the identifier of the key it certifies and
//!   names the identifier of the key that signed it;
//! * a validity period starts one hour BEFORE the instant the certificate was issued, unless
//!   the request names a customized start;
//! * the refusals of this path carry Amazon Web Services Private CA's own fixed sentences,
//!   with no ARN or status name interpolated into them.
//!
//! The rows of each table differ along what its rule generalises over: the window the CA's
//! certificate holds and the `ValidityPeriodType` the request states, the member a repeat
//! differs in, the template kind and its path length, and the way a certification request
//! fails to be one. Every call goes over the wire through the harness in
//! [`super::private_ca_lifecycle`].

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::Digest;
use x509_cert::der::asn1::OctetString;
use x509_cert::der::{Decode, Encode};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{AuthorityKeyIdentifier, BasicConstraints, SubjectAltName};
use x509_cert::ext::Extension;
use x509_cert::name::Name;
use x509_cert::Certificate;

use crate::private_ca_lifecycle::{
    active_ca, blob, certificate_of, csr_pem, pem, pem_of_bytes, request_of, root_ca_request,
    string, subject_common_name, wait_for_the_clock, Emulator,
};
use crate::resource_policy::error_message;

/// The template that produces a ROOT CA's own certificate.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// The `basicConstraints` extension, RFC 5280 section 4.2.1.9.
const BASIC_CONSTRAINTS_OID: &str = "2.5.29.19";

/// The `subjectAltName` extension, RFC 5280 section 4.2.1.6.
const SUBJECT_ALT_NAME_OID: &str = "2.5.29.17";

// The sentences Amazon Web Services Private CA answers these refusals with, spelled out
// here rather than read from the crate, so a re-authored sentence fails this suite.
/// What it answers an issuance whose validity runs past the issuing CA's own certificate.
const VALIDITY_EXCEEDS_CA_SENTENCE: &str =
    "The certificate validity specified exceeds the certificate authority validity.";
/// What it answers a `Csr` member it cannot decode.
const MALFORMED_CSR_SENTENCE: &str = "Malformed CSR.";
/// What it answers a deletion of a CA in a status it cannot be deleted from.
const DELETE_STATE_SENTENCE: &str =
    "The certificate authority is not in a valid state for deletion.";
/// What it answers an import onto a CA that cannot take a certificate.
const IMPORT_STATE_SENTENCE: &str = "The certificate authority is not in a valid state for \
                                    importing a certificate authority certificate.";
/// What it answers a write to a CA waiting to be restored.
const DELETED_STATE_SENTENCE: &str = "The certificate authority is in the DELETED state and \
                                      must be restored to complete this action.";

/// The basic constraints a template's definition states: the `cA` flag with the
/// `pathLenConstraint` beside it, or `None` for a template that defines no such extension.
type ExpectedConstraints = Option<(bool, Option<u8>)>;

/// One row of the template table: the case, the `TemplateArn` the request names, and the
/// basic constraints the certificate issued under it must carry.
type TemplateRow<'a> = (&'a str, Option<&'a str>, ExpectedConstraints);

/// A ROOT CA activated with a certificate of its OWN making, valid for `validity`.
///
/// The documented root-CA procedure — create the CA, read its certification request, issue
/// it under the root CA certificate template, import what comes back — which is how a suite
/// chooses the window the CA's certificate holds: the certificate a CA signs for itself is
/// the one issuance no window bounds, because it is the issuance that SETS the window.
pub(crate) async fn root_ca_valid_for(
    emulator: &Emulator,
    case: &str,
    common_name: &str,
    validity: Value,
) -> String {
    let created = emulator
        .ok(case, "CreateCertificateAuthority", &root_ca_request(common_name))
        .await;
    let arn = string(&created, "CertificateAuthorityArn", case);
    wait_for_the_clock();
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
    let issued = emulator
        .ok(
            case,
            "IssueCertificate",
            &json!({
                "CertificateAuthorityArn": arn,
                "Csr": blob(&csr),
                "SigningAlgorithm": "SHA256WITHRSA",
                "TemplateArn": ROOT_CA_TEMPLATE,
                "Validity": validity,
            }),
        )
        .await;
    let certificate_arn = string(&issued, "CertificateArn", case);
    wait_for_the_clock();
    let certificate = string(
        &emulator
            .ok(
                case,
                "GetCertificate",
                &json!({
                    "CertificateAuthorityArn": arn,
                    "CertificateArn": certificate_arn,
                }),
            )
            .await,
        "Certificate",
        case,
    );
    emulator
        .ok(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({"CertificateAuthorityArn": arn, "Certificate": blob(&certificate)}),
        )
        .await;
    arn
}

/// The `CertificateAuthority` structure the service reports for `arn`.
async fn described(emulator: &Emulator, case: &str, arn: &str) -> Value {
    emulator
        .ok(
            case,
            "DescribeCertificateAuthority",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await["CertificateAuthority"]
        .clone()
}

/// The reported `NotAfter` of `arn`, in whole seconds since the Unix epoch.
async fn reported_not_after(emulator: &Emulator, case: &str, arn: &str) -> i64 {
    described(emulator, case, arn).await["NotAfter"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: an active CA reports a NotAfter")) as i64
}

/// The certificate `arn` was activated with, read back through the service.
async fn own_certificate(emulator: &Emulator, case: &str, arn: &str) -> Certificate {
    let read = emulator
        .ok(
            case,
            "GetCertificateAuthorityCertificate",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await;
    certificate_of(&string(&read, "Certificate", case), case)
}

/// How many certificates the store holds under `arn`.
fn held_certificates(emulator: &Emulator, arn: &str, case: &str) -> usize {
    let state = emulator.state();
    let state = state.lock();
    state
        .certificate_authorities
        .get(arn)
        .unwrap_or_else(|| panic!("case {case:?}: the store holds no CA at {arn}"))
        .certificates
        .len()
}

/// An `IssueCertificate` body for `ca_arn` over `csr`, valid for `validity`.
fn issuance(ca_arn: &str, csr: &str, validity: Value) -> Value {
    json!({
        "CertificateAuthorityArn": ca_arn,
        "Csr": blob(csr),
        "SigningAlgorithm": "SHA256WITHRSA",
        "Validity": validity,
    })
}

/// The extension `oid` of `certificate`, when it carries one.
fn extension(certificate: &Certificate, oid: &str) -> Option<Extension> {
    certificate
        .tbs_certificate
        .extensions
        .as_ref()?
        .iter()
        .find(|extension| extension.extn_id.to_string() == oid)
        .cloned()
}

/// A certificate's validity period may not run past the issuing CA's own certificate,
/// however the request expresses it — and a request refused for that reason mints nothing.
#[tokio::test]
async fn an_issued_certificates_validity_may_not_exceed_the_issuing_cas_own() {
    let emulator = Emulator::new();

    // Two CAs whose own certificates hold very different windows: the rule is about the
    // limit a CA's certificate sets, not about any one window.
    let ninety = root_ca_valid_for(
        &emulator,
        "a CA valid ninety days",
        "ninety-day-root",
        json!({"Type": "DAYS", "Value": 90}),
    )
    .await;
    let decade = root_ca_valid_for(
        &emulator,
        "a CA valid ten years",
        "ten-year-root",
        json!({"Type": "YEARS", "Value": 10}),
    )
    .await;

    // The limit is the CA certificate's own NotAfter, which is what the CA reports.
    for (case, arn) in [("the ninety-day CA", &ninety), ("the ten-year CA", &decade)] {
        let certificate = own_certificate(&emulator, case, arn).await;
        let authority = described(&emulator, case, arn).await;
        let window = certificate.tbs_certificate.validity;
        for (member, bound) in [
            ("NotBefore", window.not_before.to_unix_duration().as_secs()),
            ("NotAfter", window.not_after.to_unix_duration().as_secs()),
        ] {
            let reported = authority[member].as_f64().unwrap_or_default() as u64;
            assert_eq!(
                reported, bound,
                "case {case:?}: the CA reports {member} of the certificate it took, \
                 expected {bound}, got {reported}"
            );
        }
    }

    let limit = reported_not_after(&emulator, "the ninety-day CA", &ninety).await;
    let csr = csr_pem("a-bounded-leaf");

    // (case, the CA asked, the validity requested, whether it is inside the CA's window)
    let rows: Vec<(&str, &String, Value, bool)> = vec![
        ("a month inside ninety days", &ninety, json!({"Type": "DAYS", "Value": 30}), true),
        ("two months inside ninety days", &ninety, json!({"Type": "MONTHS", "Value": 2}), true),
        ("eighty-nine days", &ninety, json!({"Type": "DAYS", "Value": 89}), true),
        ("a year past ninety days", &ninety, json!({"Type": "DAYS", "Value": 365}), false),
        ("one year past ninety days", &ninety, json!({"Type": "YEARS", "Value": 1}), false),
        ("four months past ninety days", &ninety, json!({"Type": "MONTHS", "Value": 4}), false),
        // The same year the ninety-day CA refuses is inside the decade CA's window.
        ("one year inside ten years", &decade, json!({"Type": "YEARS", "Value": 1}), true),
        (
            "an absolute instant a minute before the limit",
            &ninety,
            json!({"Type": "ABSOLUTE", "Value": limit - 60}),
            true,
        ),
        (
            "an absolute instant at the limit",
            &ninety,
            json!({"Type": "ABSOLUTE", "Value": limit}),
            true,
        ),
        (
            "an absolute instant a minute past the limit",
            &ninety,
            json!({"Type": "ABSOLUTE", "Value": limit + 60}),
            false,
        ),
        (
            "an absolute instant a decade past the limit",
            &ninety,
            json!({"Type": "ABSOLUTE", "Value": limit + 10 * 365 * 24 * 3600}),
            false,
        ),
        (
            "an end date past the limit",
            &ninety,
            json!({"Type": "END_DATE", "Value": 20_351_231_235_959_i64}),
            false,
        ),
    ];

    for (case, arn, validity, inside) in &rows {
        let before = held_certificates(&emulator, arn, case);
        let body = issuance(arn, &csr, validity.clone());
        if *inside {
            let issued = emulator.ok(case, "IssueCertificate", &body).await;
            let certificate_arn = string(&issued, "CertificateArn", case);
            assert!(
                certificate_arn.starts_with(&format!("{arn}/certificate/")),
                "case {case:?}: expected a certificate of {arn:?}, got {certificate_arn:?}"
            );
            let after = held_certificates(&emulator, arn, case);
            assert_eq!(
                after,
                before + 1,
                "case {case:?}: an accepted issuance mints one certificate, got {after} \
                 against {before}"
            );
        } else {
            let refused = emulator
                .refused(case, "IssueCertificate", &body, "ValidationException")
                .await;
            let message = error_message(&refused, case);
            assert_eq!(
                message, VALIDITY_EXCEEDS_CA_SENTENCE,
                "case {case:?}: expected the validity sentence, got {message:?}"
            );
            let after = held_certificates(&emulator, arn, case);
            assert_eq!(
                after, before,
                "case {case:?}: a refused issuance must mint nothing, got {after} against \
                 {before}"
            );
        }
    }
}

/// A repeat of a request a CA has already answered is the certificate it minted then; a
/// request differing in any member that decides the certificate's content gets its own.
#[tokio::test]
async fn a_repeated_issuance_request_replays_the_certificate_it_first_minted() {
    let emulator = Emulator::new();
    let (arn, _certificate) = active_ca(&emulator, "idempotent-root").await;
    let csr = csr_pem("an-idempotent-leaf");
    let other_csr = csr_pem("another-idempotent-leaf");
    let base = issuance(&arn, &csr, json!({"Type": "DAYS", "Value": 30}));

    // One row per member a request can differ in, each a request of its own; the control
    // rows differ in nothing at all and so are the SAME request as the base.
    let mut with_token = base.clone();
    with_token["IdempotencyToken"] = json!("tok-1");
    let mut other_token = base.clone();
    other_token["IdempotencyToken"] = json!("tok-2");
    let mut another_csr = base.clone();
    another_csr["Csr"] = blob(&other_csr);
    let mut longer = base.clone();
    longer["Validity"] = json!({"Type": "DAYS", "Value": 31});
    let mut other_period = base.clone();
    other_period["Validity"] = json!({"Type": "MONTHS", "Value": 1});
    let mut templated = base.clone();
    templated["TemplateArn"] = json!("arn:aws:acm-pca:::template/EndEntityCertificate/V1");
    let mut other_algorithm = base.clone();
    other_algorithm["SigningAlgorithm"] = json!("SHA384WITHRSA");
    let mut started_later = base.clone();
    started_later["ValidityNotBefore"] =
        json!({"Type": "ABSOLUTE", "Value": 2_000_000_000_i64});
    let mut passed_through = base.clone();
    passed_through["ApiPassthrough"] = json!({"Subject": {"CommonName": "passed-through"}});

    let rows: Vec<(&str, &Value)> = vec![
        ("the request as it stands", &base),
        ("the same request under a token", &with_token),
        ("the same request under another token", &other_token),
        ("another certification request", &another_csr),
        ("a day longer", &longer),
        ("the same span in another period type", &other_period),
        ("the end-entity template named explicitly", &templated),
        ("another signing algorithm of the same family", &other_algorithm),
        ("a start of the validity period", &started_later),
        ("content to pass through", &passed_through),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, body) in &rows {
        let first = string(
            &emulator.ok(case, "IssueCertificate", body).await,
            "CertificateArn",
            case,
        );
        let held = held_certificates(&emulator, &arn, case);
        // Sent again, byte for byte: the same certificate, and nothing new in the store.
        let again = string(
            &emulator.ok(case, "IssueCertificate", body).await,
            "CertificateArn",
            case,
        );
        assert_eq!(
            again, first,
            "case {case:?}: a repeated request must replay the certificate it minted"
        );
        let still_held = held_certificates(&emulator, &arn, case);
        assert_eq!(
            still_held, held,
            "case {case:?}: a replayed request must mint nothing, got {still_held} against \
             {held}"
        );
        assert!(
            minted.insert(first.clone()),
            "case {case:?}: a request differing from every other must mint a certificate of \
             its own, got {first:?} again"
        );
    }
    assert_eq!(
        held_certificates(&emulator, &arn, "the whole table"),
        rows.len(),
        "case {:?}: the CA holds exactly one certificate per distinct request",
        "the whole table"
    );
}

/// A certificate carries the basic constraints its template defines: the CA flag of the
/// template's kind, and the path length its `PathLenN` portion publishes.
#[tokio::test]
async fn an_issued_certificate_carries_the_basic_constraints_its_template_defines() {
    let emulator = Emulator::new();
    let (arn, _certificate) = active_ca(&emulator, "template-root").await;
    let csr = csr_pem("a-templated-leaf");

    // The template kinds Amazon Web Services Private CA publishes (the `TemplateArn`
    // values of "Understanding Certificate Templates"), with the basic constraints each
    // definition states: the CA kinds with the path length their name publishes
    // ("The path length is indicated by the PathLenN portion of the ARN, where N is the CA
    // depth", `IssueCertificateRequest$TemplateArn`), the end-entity kinds with CA FALSE,
    // and the blank kinds with none at all unless their name says otherwise.
    //
    // (case, the TemplateArn named, the (CA flag, path length) expected, or None for no
    // basic constraints at all)
    let rows: Vec<TemplateRow> = vec![
        ("no template at all", None, Some((false, None))),
        (
            "the end-entity default named explicitly",
            Some("arn:aws:acm-pca:::template/EndEntityCertificate/V1"),
            Some((false, None)),
        ),
        (
            "the client-authentication end-entity kind",
            Some("arn:aws:acm-pca:::template/EndEntityClientAuthCertificate/V1"),
            Some((false, None)),
        ),
        (
            "the server-authentication end-entity kind, passing the API through",
            Some("arn:aws:acm-pca:::template/EndEntityServerAuthCertificate_APIPassthrough/V1"),
            Some((false, None)),
        ),
        (
            "the code-signing kind",
            Some("arn:aws:acm-pca:::template/CodeSigningCertificate/V1"),
            Some((false, None)),
        ),
        (
            "the OCSP-signing kind",
            Some("arn:aws:acm-pca:::template/OCSPSigningCertificate/V1"),
            Some((false, None)),
        ),
        (
            "the root CA kind",
            Some(ROOT_CA_TEMPLATE),
            Some((true, None)),
        ),
        (
            "the root CA kind, passing the API through",
            Some("arn:aws:acm-pca:::template/RootCACertificate_APIPassthrough/V1"),
            Some((true, None)),
        ),
        (
            "a subordinate CA of depth zero",
            Some("arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen0/V1"),
            Some((true, Some(0))),
        ),
        (
            "a subordinate CA of depth one",
            Some("arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen1/V1"),
            Some((true, Some(1))),
        ),
        (
            "a subordinate CA of depth two, passing the API through",
            Some("arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen2_APIPassthrough/V1"),
            Some((true, Some(2))),
        ),
        (
            "a subordinate CA of depth three, passing the CSR through",
            Some(
                "arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen3_APICSRPassthrough/V1",
            ),
            Some((true, Some(3))),
        ),
        (
            "a blank end-entity kind",
            Some("arn:aws:acm-pca:::template/BlankEndEntityCertificate_APIPassthrough/V1"),
            None,
        ),
        (
            "a blank end-entity kind that names critical basic constraints",
            Some(
                "arn:aws:acm-pca:::template/\
                 BlankEndEntityCertificate_CriticalBasicConstraints_APIPassthrough/V1",
            ),
            Some((false, None)),
        ),
        (
            "a blank subordinate CA kind that names critical basic constraints",
            Some(
                "arn:aws:acm-pca:::template/\
                 BlankSubordinateCACertificate_PathLen0_CriticalBasicConstraints_APIPassthrough/V1",
            ),
            Some((true, Some(0))),
        ),
        // A name that publishes a CA kind AND a depth keeps the depth it publishes.
        (
            "a CA kind whose name publishes a depth as well",
            Some(
                "arn:aws:acm-pca:::template/\
                 BlankRootCACertificate_PathLen2_CriticalBasicConstraints_APIPassthrough/V1",
            ),
            Some((true, Some(2))),
        ),
        // A kind this service does not know defines nothing, so nothing is invented for it —
        // not even from a depth its name happens to carry.
        (
            "a template kind outside the published ones",
            Some("arn:aws:acm-pca:::template/NoSuchCertificateKind/V1"),
            None,
        ),
        (
            "a template kind outside the published ones carrying a depth",
            Some("arn:aws:acm-pca:::template/NoSuchCertificateKind_PathLen1/V1"),
            None,
        ),
    ];

    // Issue every row first and wait for the clock ONCE, rather than once per row.
    let mut issued: Vec<(&str, ExpectedConstraints, String)> = Vec::new();
    for (case, template, expected) in &rows {
        let mut body = issuance(&arn, &csr, json!({"Type": "DAYS", "Value": 30}));
        if let Some(template) = template {
            body["TemplateArn"] = json!(template);
        }
        let certificate_arn = string(
            &emulator.ok(case, "IssueCertificate", &body).await,
            "CertificateArn",
            case,
        );
        issued.push((case, *expected, certificate_arn));
    }
    wait_for_the_clock();

    for (case, expected, certificate_arn) in issued {
        let served = emulator
            .ok(
                case,
                "GetCertificate",
                &json!({"CertificateAuthorityArn": arn, "CertificateArn": certificate_arn}),
            )
            .await;
        let certificate = certificate_of(&string(&served, "Certificate", case), case);
        let carried = extension(&certificate, BASIC_CONSTRAINTS_OID);
        match expected {
            None => assert!(
                carried.is_none(),
                "case {case:?}: a template that defines no extension must carry none, \
                 got {carried:?}"
            ),
            Some((ca, path_length)) => {
                let Some(carried) = carried else {
                    panic!("case {case:?}: expected a basicConstraints extension, got none")
                };
                assert!(
                    carried.critical,
                    "case {case:?}: basic constraints are critical, got {carried:?}"
                );
                let constraints = BasicConstraints::from_der(carried.extn_value.as_bytes())
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: expected DER basic constraints: {error}")
                    });
                assert_eq!(
                    (constraints.ca, constraints.path_len_constraint),
                    (ca, path_length),
                    "case {case:?}: expected CA={ca} pathLen={path_length:?}, got \
                     {constraints:?}"
                );
            }
        }
    }
}

/// A PKCS#10 certification request for `common_name` asking for `names` as its subject
/// alternative names, PEM-encoded.
///
/// Written by `rcgen`, so the request the service reads the names out of is built by
/// something other than the code under test.
fn csr_with_alternative_names(common_name: &str, names: &[&str]) -> String {
    let key = rcgen::KeyPair::generate().expect("a key pair");
    let mut params =
        rcgen::CertificateParams::new(names.iter().map(|name| name.to_string()).collect::<Vec<_>>())
            .expect("parameters");
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(rcgen::DnType::CommonName, common_name);
    params.distinguished_name = subject;
    params
        .serialize_request(&key)
        .expect("a certification request")
        .pem()
        .expect("the request is PEM-encodable")
}

/// A general name as the text it names: a DNS name as itself, an IPv4 address as its
/// dotted quad.
fn rendered(name: &GeneralName) -> String {
    match name {
        GeneralName::DnsName(dns) => dns.as_str().to_string(),
        GeneralName::IpAddress(address) => address
            .as_bytes()
            .iter()
            .map(u8::to_string)
            .collect::<Vec<String>>()
            .join("."),
        other => format!("{other:?}"),
    }
}

/// The subject alternative names a certification request asks for are carried into the
/// certificate issued for it, whatever they are and however many.
#[tokio::test]
async fn the_alternative_names_a_request_asks_for_are_carried_into_the_certificate() {
    let emulator = Emulator::new();
    let (arn, _certificate) = active_ca(&emulator, "alternative-name-root").await;

    // (case, the names the request asks for)
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("no alternative name at all", vec![]),
        ("one DNS name", vec!["leaf.example.com"]),
        ("two DNS names", vec!["a.example.com", "b.example.com"]),
        (
            "a DNS name and an IP address",
            vec!["c.example.com", "127.0.0.1"],
        ),
    ];

    let mut issued: Vec<(&str, Vec<&str>, String)> = Vec::new();
    for (case, names) in &rows {
        let csr = csr_with_alternative_names(&format!("leaf-{}", names.len()), names);
        let body = issuance(&arn, &csr, json!({"Type": "DAYS", "Value": 30}));
        let certificate_arn = string(
            &emulator.ok(case, "IssueCertificate", &body).await,
            "CertificateArn",
            case,
        );
        issued.push((case, names.clone(), certificate_arn));
    }
    wait_for_the_clock();

    for (case, names, certificate_arn) in issued {
        let served = emulator
            .ok(
                case,
                "GetCertificate",
                &json!({"CertificateAuthorityArn": arn, "CertificateArn": certificate_arn}),
            )
            .await;
        let certificate = certificate_of(&string(&served, "Certificate", case), case);
        let carried = extension(&certificate, SUBJECT_ALT_NAME_OID);
        if names.is_empty() {
            assert!(
                carried.is_none(),
                "case {case:?}: a request asking for no alternative name leaves the \
                 certificate without the extension, got {carried:?}"
            );
            continue;
        }
        let Some(carried) = carried else {
            panic!("case {case:?}: expected a subjectAltName extension, got none")
        };
        let alternative = SubjectAltName::from_der(carried.extn_value.as_bytes())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: expected a DER subjectAltName: {error}")
            });
        let carried_names: Vec<String> = alternative.0.iter().map(rendered).collect();
        assert_eq!(
            carried_names, names,
            "case {case:?}: expected the names the request asked for, got {carried_names:?}"
        );
    }
}

/// The refusals of the certificate path carry Amazon Web Services Private CA's own
/// sentences: one per refusal class, fixed text with no ARN or status name in it.
#[tokio::test]
async fn the_certificate_path_refuses_with_the_services_own_sentences() {
    let emulator = Emulator::new();
    let (arn, certificate) = active_ca(&emulator, "refusal-root").await;
    let real_request = request_of(&csr_pem("a-well-formed-leaf"), "a well-formed request")
        .to_der()
        .expect("a decoded certification request re-encodes");

    // Every way a `Csr` member fails to be a PKCS#10 certification request: one sentence
    // for all of them.
    let malformed: Vec<(&str, String)> = vec![
        (
            "a base64 body that is not DER at all",
            pem("CERTIFICATE REQUEST", "not a certification request"),
        ),
        (
            "a certification request cut in half",
            pem_of_bytes("CERTIFICATE REQUEST", &real_request[..real_request.len() / 2]),
        ),
        (
            "a certificate inside a request block",
            pem_of_bytes(
                "CERTIFICATE REQUEST",
                &certificate_of(&certificate, "the CA certificate")
                    .to_der()
                    .expect("a decoded certificate re-encodes"),
            ),
        ),
        (
            "a single byte",
            pem_of_bytes("CERTIFICATE REQUEST", &[0x30]),
        ),
    ];
    for (case, body) in &malformed {
        let refused = emulator
            .refused(
                case,
                "IssueCertificate",
                &issuance(&arn, body, json!({"Type": "DAYS", "Value": 30})),
                "MalformedCSRException",
            )
            .await;
        let message = error_message(&refused, case);
        assert_eq!(
            message, MALFORMED_CSR_SENTENCE,
            "case {case:?}: expected the malformed-request sentence, got {message:?}"
        );
    }

    // An ACTIVE CA already holds its certificate, so it takes no other.
    let case = "importing onto an ACTIVE CA";
    let refused = emulator
        .refused(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({"CertificateAuthorityArn": arn, "Certificate": blob(&certificate)}),
            "InvalidStateException",
        )
        .await;
    let message = error_message(&refused, case);
    assert_eq!(
        message, IMPORT_STATE_SENTENCE,
        "case {case:?}: expected the import state sentence, got {message:?}"
    );

    // And an ACTIVE CA is not deletable: it has to be disabled first.
    let case = "deleting an ACTIVE CA";
    let refused = emulator
        .refused(
            case,
            "DeleteCertificateAuthority",
            &json!({"CertificateAuthorityArn": arn, "PermanentDeletionTimeInDays": 7}),
            "InvalidStateException",
        )
        .await;
    let message = error_message(&refused, case);
    assert_eq!(
        message, DELETE_STATE_SENTENCE,
        "case {case:?}: expected the deletion state sentence, got {message:?}"
    );
    assert_eq!(
        string(&described(&emulator, case, &arn).await, "Status", case),
        "ACTIVE",
        "case {case:?}: a refused deletion must leave the CA active"
    );

    // Disabled, the same deletion is answered; the CA then waits to be restored, and what
    // it holds is not writable until it is.
    let case = "a disabled CA deleted and then asked to import";
    emulator
        .ok(
            case,
            "UpdateCertificateAuthority",
            &json!({"CertificateAuthorityArn": arn, "Status": "DISABLED"}),
        )
        .await;
    emulator
        .ok(
            case,
            "DeleteCertificateAuthority",
            &json!({"CertificateAuthorityArn": arn, "PermanentDeletionTimeInDays": 7}),
        )
        .await;
    assert_eq!(
        string(&described(&emulator, case, &arn).await, "Status", case),
        "DELETED",
        "case {case:?}: the deletion of a disabled CA is answered"
    );
    let refused = emulator
        .refused(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({"CertificateAuthorityArn": arn, "Certificate": blob(&certificate)}),
            "InvalidStateException",
        )
        .await;
    let message = error_message(&refused, case);
    assert_eq!(
        message, DELETED_STATE_SENTENCE,
        "case {case:?}: expected the restore sentence, got {message:?}"
    );
    assert_eq!(
        string(&described(&emulator, case, &arn).await, "Status", case),
        "DELETED",
        "case {case:?}: a refused import must leave the deleted CA as it was"
    );
}

// The template kinds this suite names, from the `TemplateArn` catalog
// `IssueCertificateRequest$TemplateArn` publishes: the `_APIPassthrough` and
// `_APICSRPassthrough` variants are the ones that apply an `ApiPassthrough`.
/// The prefix every published template ARN carries.
const TEMPLATE_PREFIX: &str = "arn:aws:acm-pca:::template/";

/// The `subjectKeyIdentifier` extension, RFC 5280 section 4.2.1.2.
const SUBJECT_KEY_IDENTIFIER_OID: &str = "2.5.29.14";

/// The `authorityKeyIdentifier` extension, RFC 5280 section 4.2.1.1.
const AUTHORITY_KEY_IDENTIFIER_OID: &str = "2.5.29.35";

/// The `organizationName` attribute type, RFC 4519 section 2.19.
const ORGANIZATION_OID: &str = "2.5.4.10";

/// The common name the certification request of the passthrough suite asks for.
const CSR_SUBJECT: &str = "csr-subject.pca.example.test";

/// The alternative name that request asks for.
const CSR_ALTERNATIVE_NAME: &str = "csr-san.pca.example.test";

/// The common name the `ApiPassthrough` of the passthrough suite supplies.
const API_SUBJECT: &str = "api-subject.pca.example.test";

/// The organization it supplies beside that common name — an attribute the certification
/// request does not carry at all, so a certificate carrying it took the API's subject.
const API_ORGANIZATION: &str = "Example Passthrough Org";

/// The alternative name it supplies.
const API_ALTERNATIVE_NAME: &str = "api-san.pca.example.test";

/// The template ARN of `kind`.
fn template_arn(kind: &str) -> String {
    format!("{TEMPLATE_PREFIX}{kind}/V1")
}

/// The value a distinguished name carries for the attribute type `oid`, when it carries one.
fn subject_attribute(name: &Name, oid: &str) -> Option<String> {
    name.0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find(|attribute| attribute.oid.to_string() == oid)
        .map(|attribute| String::from_utf8_lossy(attribute.value.value()).to_string())
}

/// The alternative names `certificate` carries, as the text each names.
fn alternative_names(certificate: &Certificate, case: &str) -> Vec<String> {
    let Some(carried) = extension(certificate, SUBJECT_ALT_NAME_OID) else {
        return Vec::new();
    };
    SubjectAltName::from_der(carried.extn_value.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: expected a DER subjectAltName: {error}"))
        .0
        .iter()
        .map(rendered)
        .collect()
}

/// An `ApiPassthrough` is applied only under a template variant that selects it: "An
/// APIPassthrough or APICSRPassthrough template variant must be selected, or else this
/// parameter is ignored" (`IssueCertificateRequest$ApiPassthrough`). Where it IS applied, the
/// subject and the extensions it supplies take priority over the certification request's.
///
/// Every row submits the SAME certification request and the SAME `ApiPassthrough` and differs
/// only in the `TemplateArn`, so what the assertions see is the template's decision and
/// nothing else. The rows span the kinds of that catalog: no template at all, plain kinds,
/// both passthrough variants, a blank kind an `ApiPassthrough` is the only filling of, and a
/// CA kind.
#[tokio::test]
async fn api_passthrough_applies_only_under_a_passthrough_template() {
    let emulator = Emulator::new();
    let (arn, _certificate) = active_ca(&emulator, "passthrough-root").await;

    let csr = csr_with_alternative_names(CSR_SUBJECT, &[CSR_ALTERNATIVE_NAME]);
    let passthrough = json!({
        "Subject": {"CommonName": API_SUBJECT, "Organization": API_ORGANIZATION},
        "Extensions": {"SubjectAlternativeNames": [{"DnsName": API_ALTERNATIVE_NAME}]},
    });

    // (case, the template kind named — or none at all —, whether it applies the passthrough)
    let rows: Vec<(&str, Option<&str>, bool)> = vec![
        ("no template at all", None, false),
        ("the default end-entity kind", Some("EndEntityCertificate"), false),
        (
            "the server-authentication kind",
            Some("EndEntityServerAuthCertificate"),
            false,
        ),
        (
            "the end-entity kind passing the API through",
            Some("EndEntityCertificate_APIPassthrough"),
            true,
        ),
        (
            "the end-entity kind passing the API and the CSR through",
            Some("EndEntityCertificate_APICSRPassthrough"),
            true,
        ),
        (
            "the client-authentication kind passing the API through",
            Some("EndEntityClientAuthCertificate_APIPassthrough"),
            true,
        ),
        (
            "a blank kind, which an ApiPassthrough is the whole content of",
            Some("BlankEndEntityCertificate_APIPassthrough"),
            true,
        ),
        (
            "a subordinate CA kind passing the API through",
            Some("SubordinateCACertificate_PathLen1_APIPassthrough"),
            true,
        ),
    ];

    // Issue every row first and wait for the clock ONCE, rather than once per row.
    let mut issued: Vec<(&str, bool, String)> = Vec::with_capacity(rows.len());
    for (case, kind, applies) in &rows {
        let mut body = issuance(&arn, &csr, json!({"Type": "DAYS", "Value": 30}));
        body["ApiPassthrough"] = passthrough.clone();
        if let Some(kind) = kind {
            body["TemplateArn"] = json!(template_arn(kind));
        }
        let certificate_arn = string(
            &emulator.ok(case, "IssueCertificate", &body).await,
            "CertificateArn",
            case,
        );
        issued.push((case, *applies, certificate_arn));
    }
    wait_for_the_clock();

    for (case, applies, certificate_arn) in issued {
        let served = emulator
            .ok(
                case,
                "GetCertificate",
                &json!({"CertificateAuthorityArn": arn, "CertificateArn": certificate_arn}),
            )
            .await;
        let certificate = certificate_of(&string(&served, "Certificate", case), case);
        let subject = &certificate.tbs_certificate.subject;
        let common_name = subject_common_name(subject);
        let organization = subject_attribute(subject, ORGANIZATION_OID);
        let names = alternative_names(&certificate, case);
        if applies {
            assert_eq!(
                common_name, API_SUBJECT,
                "case {case:?}: expected the API-supplied common name, got {common_name:?}"
            );
            assert_eq!(
                organization.as_deref(),
                Some(API_ORGANIZATION),
                "case {case:?}: expected the API-supplied organization, got {organization:?}"
            );
            assert_eq!(
                names,
                vec![API_ALTERNATIVE_NAME.to_string()],
                "case {case:?}: expected the API-supplied alternative name, got {names:?}"
            );
        } else {
            assert_eq!(
                common_name, CSR_SUBJECT,
                "case {case:?}: expected the certification request's common name, got \
                 {common_name:?}"
            );
            assert_eq!(
                organization, None,
                "case {case:?}: a template that ignores the ApiPassthrough carries none of \
                 its attributes, got {organization:?}"
            );
            assert_eq!(
                names,
                vec![CSR_ALTERNATIVE_NAME.to_string()],
                "case {case:?}: expected the certification request's alternative name, got \
                 {names:?}"
            );
        }
    }
}

/// The identifier of the key `certificate` holds, derived the way RFC 7093 section 2's first
/// method derives one: the leftmost 160 bits of the SHA-256 digest over its
/// `subjectPublicKey` BIT STRING. Computed here from the key, not read off anything the
/// service wrote.
fn derived_key_identifier(certificate: &Certificate) -> Vec<u8> {
    sha2::Sha256::digest(
        certificate
            .tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .raw_bytes(),
    )[..20]
        .to_vec()
}

/// The key identifier `certificate` publishes as its subject key identifier, when it
/// publishes one.
fn published_key_identifier(certificate: &Certificate, case: &str) -> Option<Vec<u8>> {
    let carried = extension(certificate, SUBJECT_KEY_IDENTIFIER_OID)?;
    assert!(
        !carried.critical,
        "case {case:?}: a subject key identifier is not critical, got {carried:?}"
    );
    Some(
        OctetString::from_der(carried.extn_value.as_bytes())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: expected a KeyIdentifier OCTET STRING: {error}")
            })
            .as_bytes()
            .to_vec(),
    )
}

/// The key identifier `certificate` names as the authority that signed it, when it names one.
fn authority_key_identifier(certificate: &Certificate, case: &str) -> Option<Vec<u8>> {
    let carried = extension(certificate, AUTHORITY_KEY_IDENTIFIER_OID)?;
    assert!(
        !carried.critical,
        "case {case:?}: an authority key identifier is not critical, got {carried:?}"
    );
    AuthorityKeyIdentifier::from_der(carried.extn_value.as_bytes())
        .unwrap_or_else(|error| {
            panic!("case {case:?}: expected an AuthorityKeyIdentifier: {error}")
        })
        .key_identifier
        .map(|identifier| identifier.as_bytes().to_vec())
}

/// Every certificate a CA writes publishes the identifier of the key it certifies, and names
/// the identifier of the key it was signed with: a relying party builds a path, and matches a
/// CRL to its issuer, by those identifiers (RFC 5280 sections 4.2.1.1 and 4.2.1.2).
///
/// The rows span what the rule generalises over: the CA's OWN self-signed certificate, which
/// certifies the very key it is signed with, and the leaves it then issues under several
/// template kinds, each over a certification request of its own — so each row's subject key
/// identifier is the identifier of a different key.
#[tokio::test]
async fn every_issued_certificate_binds_its_own_key_and_the_key_that_signed_it() {
    let emulator = Emulator::new();
    let case = "a root CA of its own making";
    // The documented root-CA procedure, so the CA certificate under test is one this service
    // wrote rather than one the suite handed it.
    let arn = root_ca_valid_for(
        &emulator,
        case,
        "key-identifier-root",
        json!({"Type": "YEARS", "Value": 10}),
    )
    .await;
    let own = own_certificate(&emulator, case, &arn).await;

    let own_identifier = published_key_identifier(&own, case).unwrap_or_else(|| {
        panic!("case {case:?}: expected the CA's certificate to publish a subject key identifier")
    });
    assert_eq!(
        own_identifier,
        derived_key_identifier(&own),
        "case {case:?}: expected the identifier of the key the CA's certificate holds"
    );
    // A root CA's certificate is signed by the very key it certifies, so the two identifiers
    // on it are the same value.
    assert_eq!(
        authority_key_identifier(&own, case),
        Some(own_identifier.clone()),
        "case {case:?}: expected a self-signed root certificate to name its own key as its \
         authority"
    );

    // (case, the template kind the leaf is issued under, or none at all)
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("a leaf under no template at all", None),
        ("a leaf under the default kind", Some("EndEntityCertificate")),
        ("a code-signing leaf", Some("CodeSigningCertificate")),
        (
            "a subordinate CA certificate",
            Some("SubordinateCACertificate_PathLen0"),
        ),
    ];

    let mut issued: Vec<(&str, String)> = Vec::with_capacity(rows.len());
    for (case, kind) in &rows {
        // A certification request of its own per row, so every row certifies a different key.
        let csr = csr_pem(&format!("key-identifier-leaf-{}", issued.len()));
        let mut body = issuance(&arn, &csr, json!({"Type": "DAYS", "Value": 30}));
        if let Some(kind) = kind {
            body["TemplateArn"] = json!(template_arn(kind));
        }
        let certificate_arn = string(
            &emulator.ok(case, "IssueCertificate", &body).await,
            "CertificateArn",
            case,
        );
        issued.push((case, certificate_arn));
    }
    wait_for_the_clock();

    let mut identifiers: BTreeSet<Vec<u8>> = BTreeSet::new();
    for (case, certificate_arn) in issued {
        let served = emulator
            .ok(
                case,
                "GetCertificate",
                &json!({"CertificateAuthorityArn": arn, "CertificateArn": certificate_arn}),
            )
            .await;
        let certificate = certificate_of(&string(&served, "Certificate", case), case);
        let published = published_key_identifier(&certificate, case).unwrap_or_else(|| {
            panic!("case {case:?}: expected a subjectKeyIdentifier extension, got none")
        });
        assert_eq!(
            published,
            derived_key_identifier(&certificate),
            "case {case:?}: expected the identifier of the key the certificate holds"
        );
        assert_eq!(
            authority_key_identifier(&certificate, case),
            Some(own_identifier.clone()),
            "case {case:?}: expected the identifier the issuing CA's own certificate publishes"
        );
        identifiers.insert(published);
    }
    assert_eq!(
        identifiers.len(),
        rows.len(),
        "the rows certify different keys, so each row's identifier is its own: got \
         {identifiers:?}"
    );
}

/// How far before the issuance instant a validity period starts when the request names no
/// start of its own: one hour, which is what makes a certificate usable the moment it is
/// issued by a relying party whose clock lags.
const BACKDATED_BY: u64 = 3600;

/// One row of the validity-start table.
struct ValidityRow {
    /// The case every assertion about this row names.
    case: &'static str,
    /// The `Validity` the request states.
    validity: Value,
    /// The `ValidityNotBefore` the request states, in Unix seconds, when it states one.
    start: Option<i64>,
    /// How long after the start of the validity period it ends, when the row's `Validity`
    /// states a span of whole seconds rather than a calendar one.
    period: Option<u64>,
}

/// The Unix second `time` falls in.
fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .expect("a time after the epoch")
        .as_secs()
}

/// A certificate's validity period starts BEFORE the instant it was issued — the issuance
/// instant less [`BACKDATED_BY`] — unless the request names "a customized starting time for
/// the validity period" (`IssueCertificateRequest$ValidityNotBefore`), which is then the start
/// verbatim. The period the request asked for is measured from the issuance instant either
/// way.
///
/// The rows span what the rule generalises over: the `ValidityPeriodType` the window is stated
/// in, and whether the request customizes the start.
#[tokio::test]
async fn the_validity_period_starts_before_the_issuance_unless_the_request_names_a_start() {
    let emulator = Emulator::new();
    let (arn, _certificate) = active_ca(&emulator, "backdated-root").await;
    let csr = csr_pem("a-backdated-leaf");

    // 1767225600 = 2026-01-01T00:00:00Z, the instant the customizing rows start at.
    let customized = 1_767_225_600_i64;
    let rows: Vec<ValidityRow> = vec![
        ValidityRow {
            case: "thirty days from the issuance",
            validity: json!({"Type": "DAYS", "Value": 30}),
            start: None,
            period: Some(30 * 86_400),
        },
        ValidityRow {
            case: "one day from the issuance",
            validity: json!({"Type": "DAYS", "Value": 1}),
            start: None,
            period: Some(86_400),
        },
        ValidityRow {
            case: "a calendar year from the issuance",
            validity: json!({"Type": "YEARS", "Value": 1}),
            start: None,
            period: None,
        },
        ValidityRow {
            case: "a calendar month from the issuance",
            validity: json!({"Type": "MONTHS", "Value": 1}),
            start: None,
            period: None,
        },
        ValidityRow {
            case: "thirty days from a customized start",
            validity: json!({"Type": "DAYS", "Value": 30}),
            start: Some(customized),
            period: Some(30 * 86_400),
        },
        ValidityRow {
            case: "a calendar year from a customized start",
            validity: json!({"Type": "YEARS", "Value": 1}),
            start: Some(customized),
            period: None,
        },
    ];

    // Each row's issuance is bracketed by the instants around the call, because the start it
    // must carry is derived from the instant the service issued it.
    let mut issued: Vec<(&ValidityRow, u64, u64, String)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut body = issuance(&arn, &csr, row.validity.clone());
        if let Some(start) = row.start {
            body["ValidityNotBefore"] = json!({"Type": "ABSOLUTE", "Value": start});
        }
        // Every row wants a certificate of its own over the one certification request.
        body["IdempotencyToken"] = json!(format!("backdated-{}", issued.len()));
        let before = unix_seconds(SystemTime::now());
        let certificate_arn = string(
            &emulator.ok(row.case, "IssueCertificate", &body).await,
            "CertificateArn",
            row.case,
        );
        let after = unix_seconds(SystemTime::now());
        issued.push((row, before, after, certificate_arn));
    }
    wait_for_the_clock();

    for (row, before, after, certificate_arn) in issued {
        let case = row.case;
        let served = emulator
            .ok(
                case,
                "GetCertificate",
                &json!({"CertificateAuthorityArn": arn, "CertificateArn": certificate_arn}),
            )
            .await;
        let window = certificate_of(&string(&served, "Certificate", case), case)
            .tbs_certificate
            .validity;
        let not_before = window.not_before.to_unix_duration().as_secs();
        let not_after = window.not_after.to_unix_duration().as_secs();

        match row.start {
            // A customized start is the start verbatim: nothing is subtracted from it.
            Some(start) => assert_eq!(
                i64::try_from(not_before).expect("a representable date"),
                start,
                "case {case:?}: expected the customized start {start}, got {not_before}"
            ),
            // And without one, the start is the issuance instant less the backdating, so it
            // lies that far before the instants the call was made between.
            None => assert!(
                (before - BACKDATED_BY..=after - BACKDATED_BY).contains(&not_before),
                "case {case:?}: expected a start in {}..={}, got {not_before}",
                before - BACKDATED_BY,
                after - BACKDATED_BY
            ),
        }
        assert!(
            not_after > not_before,
            "case {case:?}: expected a period that ends after it starts, got \
             {not_before}..{not_after}"
        );
        // The period the request asked for is measured from the ISSUANCE instant, which is
        // where the backdated start is measured from too.
        if let Some(period) = row.period {
            let issuance_instant = match row.start {
                Some(_) => not_before,
                None => not_before + BACKDATED_BY,
            };
            assert_eq!(
                not_after - issuance_instant,
                period,
                "case {case:?}: expected the requested period of {period} seconds from the \
                 issuance, got {}",
                not_after - issuance_instant
            );
        }
    }
}
