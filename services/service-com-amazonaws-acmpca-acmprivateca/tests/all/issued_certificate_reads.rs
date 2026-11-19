// SPDX-License-Identifier: CC0-1.0

//! Which refusal `GetCertificate` answers for a certificate it cannot serve.
//!
//! The rule under test: a read of a certificate reports on the CERTIFICATE the request
//! names, so a certificate ARN the private CA holds no record of is the absence of that
//! certificate (`ResourceNotFoundException` with the service's one fixed sentence for an
//! absent certificate), whatever status the authority rests in and whether or not it holds
//! a certificate of its OWN yet — the authority's readiness is never reported as the reason
//! the lookup found nothing.
//!
//! `RequestInProgressException` keeps the one thing it says: the certificate asked for is
//! still being minted, so the same request later succeeds.
//!
//! The rows span the statuses a CA passes through, before and after it holds its own
//! certificate, crossed with the ways a certificate ARN fails to resolve on it.

use serde_json::json;
use service_com_amazonaws_acmpca_acmprivateca::state::CERTIFICATE_NOT_FOUND_MESSAGE;

use crate::private_ca_lifecycle::{
    active_ca, blob, csr_pem, own_ca_certificate, root_ca_request, string,
    wait_for_the_clock, Emulator,
};
use crate::resource_policy::error_message;

/// The template that produces a ROOT CA's own certificate.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// The whole sentence Amazon Web Services Private CA refuses an absent certificate with:
/// fixed text, with no part of it taken from the request or from the store.
const ABSENT_CERTIFICATE_SENTENCE: &str = "Could not find specified certificate.";

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

/// A CA whose creation has not settled yet already reports the absence of a certificate it
/// has no record of: the read is about the certificate, not about the authority.
#[tokio::test]
async fn a_ca_still_being_created_reports_the_certificate_as_the_absent_one() {
    let emulator = Emulator::new();
    let case = "a CA still being created";
    let arn = string(
        &emulator
            .ok(case, "CreateCertificateAuthority", &root_ca_request("creating-root"))
            .await,
        "CertificateAuthorityArn",
        case,
    );

    for (row, certificate_arn) in unresolvable_certificate_arns(&arn) {
        let case = format!("{case}: {row}");
        reports_the_certificate_as_absent(&emulator, &case, &arn, &certificate_arn).await;
    }
}

/// Every settled status a CA rests in answers the same way, before and after it holds a
/// certificate of its own, and for a certificate ARN of ANOTHER authority too.
#[tokio::test]
async fn every_status_reports_an_unknown_certificate_as_the_absent_one() {
    let emulator = Emulator::new();
    // One CA left waiting for its certificate, one driven all the way to ACTIVE.
    let pending = string(
        &emulator
            .ok("pending", "CreateCertificateAuthority", &root_ca_request("pending-root"))
            .await,
        "CertificateAuthorityArn",
        "pending",
    );
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

    // (the stage, the CA it is read on, the status that CA is put into first)
    let stages: &[(&str, &str, Option<&str>)] = &[
        ("a CA waiting for its certificate", pending.as_str(), None),
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
/// certificate it signs for itself: that one is minted asynchronously and then served,
/// while any other ARN on the same CA is the absence of a certificate.
#[tokio::test]
async fn the_root_certificate_a_pending_ca_signs_for_itself_is_minted_then_served() {
    let emulator = Emulator::new();
    let case = "a pending CA signing for itself";
    let arn = string(
        &emulator
            .ok(case, "CreateCertificateAuthority", &root_ca_request("self-signing-root"))
            .await,
        "CertificateAuthorityArn",
        case,
    );
    wait_for_the_clock();
    let csr = string(
        &emulator
            .ok(case, "GetCertificateAuthorityCsr", &json!({"CertificateAuthorityArn": arn}))
            .await,
        "Csr",
        case,
    );
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
    let served = emulator
        .ok(case, "GetCertificate", &read(&arn, &root_arn))
        .await;
    assert!(
        string(&served, "Certificate", case).contains("-----BEGIN CERTIFICATE-----"),
        "case {case:?}: a certificate the CA did issue is served whatever status it is in"
    );

    // Any OTHER ARN on that same CA is the absence of a certificate.
    for (row, certificate_arn) in unresolvable_certificate_arns(&arn) {
        let case = format!("{case}: {row}");
        reports_the_certificate_as_absent(&emulator, &case, &arn, &certificate_arn).await;
    }
}

/// Taking its own certificate does not change WHICH refusal a CA answers for a certificate
/// it has no record of: the same code and the same sentence before and after the import.
#[tokio::test]
async fn taking_its_own_certificate_does_not_change_the_refusal() {
    let emulator = Emulator::new();
    let case = "imported";
    let arn = string(
        &emulator
            .ok(case, "CreateCertificateAuthority", &root_ca_request("imported-root"))
            .await,
        "CertificateAuthorityArn",
        case,
    );
    wait_for_the_clock();

    let rows = unresolvable_certificate_arns(&arn);
    for (row, certificate_arn) in &rows {
        let case = format!("{case}: {row}, before its certificate");
        reports_the_certificate_as_absent(&emulator, &case, &arn, certificate_arn).await;
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
        let case = format!("{case}: {row}, with its certificate");
        reports_the_certificate_as_absent(&emulator, &case, &arn, certificate_arn).await;
    }
}
