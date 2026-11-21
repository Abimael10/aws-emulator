// SPDX-License-Identifier: CC0-1.0

//! Which refusal `GetCertificate` answers for a certificate it cannot serve.
//!
//! The rule under test: a certificate read is gated on the AUTHORITY first. A private CA
//! whose key has signed nothing yet — it holds no certificate of its own and has issued
//! none, so it cannot have issued the certificate the request names — is refused for its
//! STATE (HTTP 400 `InvalidStateException`, fixed text around the `CertificateAuthorityArn`
//! the request carried), before the certificate ARN is resolved on it. Once that CA has
//! signed something, a certificate ARN it holds no record of is the absence of that
//! CERTIFICATE instead (`ResourceNotFoundException` with the service's one fixed sentence,
//! which names no ARN), whatever status the authority rests in.
//!
//! `RequestInProgressException` keeps the one thing it says: the certificate asked for is
//! still being minted, so the same request later succeeds.
//!
//! The rows span the statuses a CA passes through, before and after its key has signed
//! anything, crossed with the ways a certificate ARN fails to resolve on it.

use serde_json::json;
use service_com_amazonaws_acmpca_acmprivateca::state::{
    not_in_state_to_have_issued_message, CERTIFICATE_NOT_FOUND_MESSAGE,
};

use crate::private_ca_lifecycle::{
    active_ca, blob, csr_pem, own_ca_certificate, root_ca_request, string, wait_for_the_clock,
    Emulator,
};
use crate::resource_policy::error_message;

/// The template that produces a ROOT CA's own certificate.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// The whole sentence Amazon Web Services Private CA refuses an absent certificate with:
/// fixed text, with no part of it taken from the request or from the store.
const ABSENT_CERTIFICATE_SENTENCE: &str = "Could not find specified certificate.";

/// The whole sentence Amazon Web Services Private CA refuses a certificate read on an
/// authority that cannot have issued certificates with: fixed text around the
/// `CertificateAuthorityArn` the request carried, which is the only value in it.
fn not_ready_sentence(ca_arn: &str) -> String {
    format!(
        "The certificate authority {ca_arn} is not in the correct state to have issued \
         certificates."
    )
}

/// The ways a certificate ARN under `ca_arn` names no certificate that CA holds: serials of
/// different lengths and spellings that the CA never allocated.
fn unresolvable_certificate_arns(ca_arn: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "a 32-character serial the CA never allocated",
            format!("{ca_arn}/certificate/0123456789abcdef0123456789abcdef"),
        ),
        (
            "a shorter serial",
            format!("{ca_arn}/certificate/0123456789abcdef"),
        ),
        (
            "a serial spelled in upper-case hex",
            format!("{ca_arn}/certificate/0123456789ABCDEF0123456789ABCDEF"),
        ),
    ]
}

/// A `GetCertificate` body.
fn read(ca_arn: &str, certificate_arn: &str) -> serde_json::Value {
    json!({"CertificateAuthorityArn": ca_arn, "CertificateArn": certificate_arn})
}

/// Create a CA and leave it where creation leaves it, answering its ARN.
async fn created_ca(emulator: &Emulator, common_name: &str) -> String {
    let created = emulator
        .ok(common_name, "CreateCertificateAuthority", &root_ca_request(common_name))
        .await;
    string(&created, "CertificateAuthorityArn", common_name)
}

/// Require `GetCertificate` to report the absence of the CERTIFICATE, with the service's
/// fixed sentence and nothing of the request inside it.
async fn reports_the_certificate_as_absent(
    emulator: &Emulator,
    case: &str,
    ca_arn: &str,
    certificate_arn: &str,
) {
    let refused = emulator
        .refused(
            case,
            "GetCertificate",
            &read(ca_arn, certificate_arn),
            "ResourceNotFoundException",
        )
        .await;
    let message = error_message(&refused, case);
    assert_eq!(
        message, ABSENT_CERTIFICATE_SENTENCE,
        "case {case:?}: expected the sentence about the certificate, got {message:?}"
    );
    assert!(
        !message.contains(ca_arn) && !message.contains(certificate_arn),
        "case {case:?}: the sentence is fixed text and names no ARN, got {message:?}"
    );
}

/// Require `GetCertificate` to report the AUTHORITY's state, with the service's sentence
/// for it built out of the CA ARN the request carried.
async fn reports_the_authority_as_not_ready(
    emulator: &Emulator,
    case: &str,
    ca_arn: &str,
    certificate_arn: &str,
) {
    let refused = emulator
        .refused(
            case,
            "GetCertificate",
            &read(ca_arn, certificate_arn),
            "InvalidStateException",
        )
        .await;
    let message = error_message(&refused, case);
    assert_eq!(
        message,
        not_ready_sentence(ca_arn),
        "case {case:?}: expected the sentence about the authority, got {message:?}"
    );
    assert!(
        message.contains(ca_arn) && !message.contains("/certificate/"),
        "case {case:?}: the only value in the sentence is the CA ARN, asked for \
         {certificate_arn:?}, got {message:?}"
    );
}

/// The service's constant and the sentence it has to spell are one string, so a reworded
/// constant cannot pass unnoticed.
#[test]
fn the_absent_certificate_sentence_is_the_services_fixed_text() {
    assert_eq!(
        CERTIFICATE_NOT_FOUND_MESSAGE, ABSENT_CERTIFICATE_SENTENCE,
        "the absent-certificate refusal carries one fixed sentence, got \
         {CERTIFICATE_NOT_FOUND_MESSAGE:?}"
    );
}

/// The state refusal's sentence is fixed text with ONE value interpolated — the CA ARN —
/// so it reads differently for two authorities and the same for one.
#[test]
fn the_not_ready_sentence_interpolates_only_the_authority_arn() {
    let arns = [
        "arn:aws:acm-pca:eu-west-1:000000000000:certificate-authority/\
         11111111-2222-4333-8444-555555555555",
        "arn:aws:acm-pca:us-east-1:123456789012:certificate-authority/short-id",
    ];
    for arn in arns {
        let message = not_in_state_to_have_issued_message(arn);
        assert_eq!(
            message,
            not_ready_sentence(arn),
            "case {arn:?}: expected the service's state sentence, got {message:?}"
        );
    }
    assert_ne!(
        not_in_state_to_have_issued_message(arns[0]),
        not_in_state_to_have_issued_message(arns[1]),
        "the sentence carries the CA ARN of the request it answers"
    );
}

/// A CA whose key has signed nothing — still being created, or settled and waiting for its
/// certificate — answers for the AUTHORITY, whichever certificate ARN is asked of it.
#[tokio::test]
async fn a_ca_that_has_signed_nothing_is_refused_for_its_state() {
    let emulator = Emulator::new();
    // Two authorities: one read while creation has not settled yet, one read after it has.
    let creating = created_ca(&emulator, "creating-root").await;
    let pending = created_ca(&emulator, "pending-root").await;
    // A certificate that really exists, issued by a third CA, so the foreign-ARN row below
    // is about an ARN the service really resolves elsewhere.
    let (active, _) = active_ca(&emulator, "signed-neighbour").await;
    let case = "a leaf of the active CA";
    let issued = emulator
        .ok(
            case,
            "IssueCertificate",
            &json!({
                "CertificateAuthorityArn": active,
                "Csr": blob(&csr_pem("an-end-entity")),
                "SigningAlgorithm": "SHA256WITHRSA",
                "Validity": {"Type": "DAYS", "Value": 30},
            }),
        )
        .await;
    let leaf = string(&issued, "CertificateArn", case);

    // (the stage, the CA it is read on, whether the clock-owed CREATING ->
    // PENDING_CERTIFICATE transition has come due by then)
    let stages: &[(&str, &str, bool)] = &[
        ("a CA still being created", creating.as_str(), false),
        ("a CA waiting for its certificate", pending.as_str(), true),
    ];
    for (stage, ca_arn, settled) in stages {
        if *settled {
            wait_for_the_clock();
        }
        let mut rows = unresolvable_certificate_arns(ca_arn);
        rows.push(("a certificate another authority issued", leaf.clone()));
        for (row, certificate_arn) in rows {
            let case = format!("{stage}: {row}");
            reports_the_authority_as_not_ready(&emulator, &case, ca_arn, &certificate_arn).await;
        }
    }
}

/// Every status a CA that HAS signed something rests in answers the absence of the
/// CERTIFICATE: once the key has been used, an unknown ARN is a missing certificate rather
/// than an authority in the wrong state.
#[tokio::test]
async fn a_ca_that_has_signed_reports_an_unknown_certificate_as_the_absent_one() {
    let emulator = Emulator::new();
    let (active, _) = active_ca(&emulator, "served-root").await;

    // A certificate that really exists — issued by the ACTIVE CA — so the foreign-ARN row
    // below is about which authority holds the certificate rather than about a made-up ARN.
    let case = "a leaf of the active CA";
    let issued = emulator
        .ok(
            case,
            "IssueCertificate",
            &json!({
                "CertificateAuthorityArn": active,
                "Csr": blob(&csr_pem("an-end-entity")),
                "SigningAlgorithm": "SHA256WITHRSA",
                "Validity": {"Type": "DAYS", "Value": 30},
            }),
        )
        .await;
    let leaf = string(&issued, "CertificateArn", case);
    wait_for_the_clock();
    emulator.ok(case, "GetCertificate", &read(&active, &leaf)).await;

    // A CA still waiting for its certificate, which has nonetheless signed one: the root
    // certificate it signs for itself. Its key has been used, so it reads like the others.
    let signing = created_ca(&emulator, "self-signing-root").await;
    wait_for_the_clock();
    let csr = string(
        &emulator
            .ok(
                "a pending CA signing for itself",
                "GetCertificateAuthorityCsr",
                &json!({"CertificateAuthorityArn": signing}),
            )
            .await,
        "Csr",
        "a pending CA signing for itself",
    );
    emulator
        .ok(
            "a pending CA signing for itself",
            "IssueCertificate",
            &json!({
                "CertificateAuthorityArn": signing,
                "Csr": blob(&csr),
                "SigningAlgorithm": "SHA256WITHRSA",
                "TemplateArn": ROOT_CA_TEMPLATE,
                "Validity": {"Type": "YEARS", "Value": 3},
            }),
        )
        .await;

    // (the stage, the CA it is read on, the status that CA is put into first)
    let stages: &[(&str, &str, Option<&str>)] = &[
        ("a CA that signed its own root certificate", signing.as_str(), None),
        ("an active CA", active.as_str(), None),
        ("a disabled CA", active.as_str(), Some("DISABLED")),
        ("an active CA again", active.as_str(), Some("ACTIVE")),
    ];

    for (stage, ca_arn, status) in stages {
        if let Some(status) = status {
            emulator
                .ok(
                    stage,
                    "UpdateCertificateAuthority",
                    &json!({"CertificateAuthorityArn": ca_arn, "Status": status}),
                )
                .await;
        }
        let mut rows = unresolvable_certificate_arns(ca_arn);
        if *ca_arn != active.as_str() {
            // A certificate another authority issued is one this CA holds no record of.
            rows.push(("a certificate another authority issued", leaf.clone()));
        }
        for (row, certificate_arn) in rows {
            let case = format!("{stage}: {row}");
            reports_the_certificate_as_absent(&emulator, &case, ca_arn, &certificate_arn).await;
        }
        // The certificate the active CA did issue is still served in that status.
        emulator
            .ok(stage, "GetCertificate", &read(&active, &leaf))
            .await;
    }
}

/// The ONE certificate a CA without its own certificate can have issued is the root
/// certificate it signs for itself: before it is asked for one the read is the authority's
/// state refusal, that certificate is then minted asynchronously and served, and any other
/// ARN on the same CA is the absence of a certificate from then on.
#[tokio::test]
async fn the_root_certificate_a_pending_ca_signs_for_itself_is_minted_then_served() {
    let emulator = Emulator::new();
    let case = "a pending CA signing for itself";
    let arn = created_ca(&emulator, "self-signing-root").await;
    wait_for_the_clock();
    let csr = string(
        &emulator
            .ok(case, "GetCertificateAuthorityCsr", &json!({"CertificateAuthorityArn": arn}))
            .await,
        "Csr",
        case,
    );

    // Before it has signed anything, every certificate ARN on it is the state refusal.
    for (row, certificate_arn) in unresolvable_certificate_arns(&arn) {
        let row = format!("{case}: {row}, before it signed anything");
        reports_the_authority_as_not_ready(&emulator, &row, &arn, &certificate_arn).await;
    }

    let root_arn = string(
        &emulator
            .ok(
                case,
                "IssueCertificate",
                &json!({
                    "CertificateAuthorityArn": arn,
                    "Csr": blob(&csr),
                    "SigningAlgorithm": "SHA256WITHRSA",
                    "TemplateArn": ROOT_CA_TEMPLATE,
                    "Validity": {"Type": "YEARS", "Value": 3},
                }),
            )
            .await,
        "CertificateArn",
        case,
    );

    // Until that issuance completes, the retry signal is the one that says so.
    let in_progress = emulator
        .refused(
            case,
            "GetCertificate",
            &read(&arn, &root_arn),
            "RequestInProgressException",
        )
        .await;
    assert!(
        error_message(&in_progress, case).contains(&root_arn),
        "case {case:?}: the in-progress refusal is about the certificate being minted"
    );
    wait_for_the_clock();
    // The documented way a root CA is activated reads its own certificate back BEFORE it
    // is imported, so the CA still waits for its certificate here.
    let served = emulator
        .ok(case, "GetCertificate", &read(&arn, &root_arn))
        .await;
    assert!(
        string(&served, "Certificate", case).contains("-----BEGIN CERTIFICATE-----"),
        "case {case:?}: a certificate the CA did issue is served whatever status it is in"
    );

    // Any OTHER ARN on that same CA is the absence of a certificate now.
    for (row, certificate_arn) in unresolvable_certificate_arns(&arn) {
        let row = format!("{case}: {row}, after it signed one");
        reports_the_certificate_as_absent(&emulator, &row, &arn, &certificate_arn).await;
    }
}

/// Taking its own certificate is the other way a CA's key becomes one that could have
/// issued certificates: the same ARN that was the authority's state refusal before the
/// import is the absence of a certificate after it.
#[tokio::test]
async fn taking_its_own_certificate_changes_which_refusal_the_read_answers() {
    let emulator = Emulator::new();
    let case = "imported";
    let arn = created_ca(&emulator, "imported-root").await;
    wait_for_the_clock();

    let rows = unresolvable_certificate_arns(&arn);
    for (row, certificate_arn) in &rows {
        let row = format!("{case}: {row}, before its certificate");
        reports_the_authority_as_not_ready(&emulator, &row, &arn, certificate_arn).await;
    }

    let own = own_ca_certificate(&emulator, case, &arn).await;
    emulator
        .ok(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({"CertificateAuthorityArn": arn, "Certificate": blob(&own)}),
        )
        .await;

    for (row, certificate_arn) in &rows {
        let row = format!("{case}: {row}, with its certificate");
        reports_the_certificate_as_absent(&emulator, &row, &arn, certificate_arn).await;
    }
}
