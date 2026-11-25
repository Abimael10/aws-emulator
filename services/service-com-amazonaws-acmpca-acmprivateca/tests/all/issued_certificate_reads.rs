// SPDX-License-Identifier: CC0-1.0

//! Which refusal `GetCertificate` answers for a certificate it cannot serve.
//!
//! The rule under test: once the private CA a request names has been resolved, a certificate
//! read reports on the CERTIFICATE the request names, which it resolves on that CA BEFORE
//! the authority's own status is judged — so a certificate ARN the CA holds no record of is
//! the absence of that certificate (HTTP 400 `ResourceNotFoundException` with the service's
//! one fixed sentence, which names no ARN), in every status the CA still serves its
//! certificates in, whether or not its key has signed anything yet. Which refusal a read
//! answers therefore does NOT change as the CA is activated. The sentence about the
//! AUTHORITY's state is reserved for a CA that serves none of its certificates at all — one
//! withdrawn from service by a deletion until it is restored — where it is answered for the
//! certificate the CA does hold as much as for one it does not, which is the same stage
//! `RevokeCertificate` judges the authority at.
//!
//! `RequestInProgressException` keeps the one thing it says: the certificate asked for is
//! still being minted, so the same request later succeeds.
//!
//! The rows differ along what the rule generalises over: the status the CA rests in (every
//! member `CertificateAuthorityStatus` declares), the stages a CA passes through before and
//! after its key has signed anything, and the way a certificate ARN fails to resolve on it
//! (a serial it never allocated, a shorter one, another spelling of one, a serial another
//! authority allocated, and another authority's certificate ARN).

use serde_json::json;
use service_com_amazonaws_acmpca_acmprivateca::ops::types::CERTIFICATE_AUTHORITY_STATUS_VALUES;
use service_com_amazonaws_acmpca_acmprivateca::state::{
    not_in_state_to_have_issued_message, CERTIFICATE_NOT_FOUND_MESSAGE, STATUS_ACTIVE,
    STATUS_CREATING, STATUS_DELETED, STATUS_DISABLED, STATUS_EXPIRED, STATUS_FAILED,
    STATUS_PENDING_CERTIFICATE,
};

use crate::delete_certificate_authority::force_status;
use crate::private_ca_lifecycle::{
    active_ca, blob, csr_pem, own_ca_certificate, root_ca_request, serial_in, string,
    wait_for_the_clock, Emulator,
};
use crate::resource_policy::error_message;

/// The template that produces a ROOT CA's own certificate.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// The whole sentence Amazon Web Services Private CA refuses an absent certificate with:
/// fixed text, with no part of it taken from the request or from the store.
const ABSENT_CERTIFICATE_SENTENCE: &str = "Could not find specified certificate.";

/// The whole sentence Amazon Web Services Private CA refuses a certificate read on an
/// authority that serves none of its certificates with: fixed text around the
/// `CertificateAuthorityArn` the request carried, which is the only value in it.
fn not_ready_sentence(ca_arn: &str) -> String {
    format!(
        "The certificate authority {ca_arn} is not in the correct state to have issued \
         certificates."
    )
}

/// Every `CertificateAuthorityStatus` the model declares, against whether a CA resting in it
/// still SERVES the certificates it holds.
///
/// A deletion withdraws a private CA from service until it is restored — "the CA won't be
/// permanently deleted until the restoration period has passed"
/// (`DeleteCertificateAuthority` docs) — so nothing it holds is reached through it; a CA in
/// any other status answers about the certificate a request names, whether it could still
/// sign one in that status or not.
const READS_BY_STATUS: &[(&str, bool)] = &[
    (STATUS_CREATING, true),
    (STATUS_PENDING_CERTIFICATE, true),
    (STATUS_ACTIVE, true),
    (STATUS_DISABLED, true),
    (STATUS_EXPIRED, true),
    (STATUS_FAILED, true),
    (STATUS_DELETED, false),
];

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

/// Have `ca_arn` issue an end-entity certificate, answering its ARN once the issuance the
/// clock owed has come due.
async fn issued_certificate(emulator: &Emulator, case: &str, ca_arn: &str, label: &str) -> String {
    let issued = emulator
        .ok(
            case,
            "IssueCertificate",
            &json!({
                "CertificateAuthorityArn": ca_arn,
                "Csr": blob(&csr_pem(label)),
                "SigningAlgorithm": "SHA256WITHRSA",
                "Validity": {"Type": "DAYS", "Value": 30},
                "IdempotencyToken": label,
            }),
        )
        .await;
    let arn = string(&issued, "CertificateArn", case);
    // A test may wait for a due instant; a handler may not.
    wait_for_the_clock();
    arn
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

/// Require `GetCertificate` to report the AUTHORITY's state, with the service's sentence for
/// it built out of the CA ARN the request carried.
async fn reports_the_authority_as_withdrawn(
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

/// [`READS_BY_STATUS`] spans the whole enum, so a member the model gains cannot go untested.
#[test]
fn the_status_table_spans_the_status_enum() {
    let mut covered: Vec<&str> = READS_BY_STATUS.iter().map(|(status, _)| *status).collect();
    covered.sort_unstable();
    let mut declared: Vec<&str> = CERTIFICATE_AUTHORITY_STATUS_VALUES.to_vec();
    declared.sort_unstable();
    assert_eq!(
        covered, declared,
        "the table must span every CertificateAuthorityStatus the model declares, got \
         {covered:?} against {declared:?}"
    );
}

/// Every status a private CA can rest in, crossed with every way a certificate ARN fails to
/// resolve on it: the answer names the CERTIFICATE while the CA still serves its
/// certificates, and names the AUTHORITY — for the certificate it does hold as much as for
/// one it does not — once a deletion has withdrawn it from service.
#[tokio::test]
async fn an_unknown_certificate_is_not_found_while_the_ca_still_serves_its_certificates() {
    let emulator = Emulator::new();
    let case = "certificate reads by status";
    let (ca_arn, _certificate) = active_ca(&emulator, "read-statuses").await;
    let (other_arn, _other) = active_ca(&emulator, "another-authority").await;
    let foreign = issued_certificate(&emulator, case, &other_arn, "foreign-leaf").await;
    let held = issued_certificate(&emulator, case, &ca_arn, "held-leaf").await;
    // The certificate body and chain the CA serves for it while it is ACTIVE, which is what
    // every serving status below has to keep answering, byte for byte.
    let issued = emulator
        .ok(case, "GetCertificate", &read(&ca_arn, &held))
        .await;
    let certificate = string(&issued, "Certificate", case);
    assert!(
        certificate.contains("-----BEGIN CERTIFICATE-----"),
        "case {case:?}: expected the PEM the CA issued, got {certificate:?}"
    );

    for (status, serves) in READS_BY_STATUS {
        let row = format!("a certificate read on a {status} CA");
        force_status(&emulator, &ca_arn, status, &row);

        let mut rows = unresolvable_certificate_arns(&ca_arn);
        rows.push((
            "a serial another authority allocated, presented under this CA",
            format!(
                "{ca_arn}/certificate/{}",
                serial_in(&foreign, "the neighbour's certificate")
            ),
        ));
        rows.push(("another authority's certificate ARN", foreign.clone()));

        // A CA still in service answers for the certificate the request named; one withdrawn
        // from service answers for itself, before any certificate ARN is resolved.
        for (named, certificate_arn) in &rows {
            let case = format!("{row} naming {named}");
            if *serves {
                reports_the_certificate_as_absent(&emulator, &case, &ca_arn, certificate_arn)
                    .await;
            } else {
                reports_the_authority_as_withdrawn(&emulator, &case, &ca_arn, certificate_arn)
                    .await;
            }
        }

        // The certificate the CA did issue is served in every status it still serves its
        // certificates in, and is refused for the AUTHORITY in the one status that serves
        // none — which is what says the authority is judged before the certificate is
        // resolved there, and after it everywhere else.
        let case = format!("{row}: the certificate the CA holds");
        if *serves {
            let served = emulator
                .ok(&case, "GetCertificate", &read(&ca_arn, &held))
                .await;
            assert_eq!(
                served, issued,
                "case {case:?}: expected the certificate the CA issued, served as it was \
                 stored, got {served:?}"
            );
        } else {
            reports_the_authority_as_withdrawn(&emulator, &case, &ca_arn, &held).await;
        }
    }
}

/// A CA whose key has signed nothing — still being created, or settled and waiting for its
/// certificate — answers for the CERTIFICATE, whichever certificate ARN is asked of it.
#[tokio::test]
async fn a_ca_that_has_signed_nothing_reports_an_unknown_certificate_as_the_absent_one() {
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
            reports_the_certificate_as_absent(&emulator, &case, ca_arn, &certificate_arn).await;
        }
    }
}

/// Every status a CA that HAS signed something rests in answers the absence of the
/// CERTIFICATE too, and the certificate it did issue stays served through them.
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
/// certificate it signs for itself: before it is asked for one every ARN on it is the
/// absence of a certificate, that certificate is then minted asynchronously and served, and
/// any other ARN on the same CA is still the absence of a certificate.
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

    // Before it has signed anything, every certificate ARN on it is an absent certificate.
    for (row, certificate_arn) in unresolvable_certificate_arns(&arn) {
        let row = format!("{case}: {row}, before it signed anything");
        reports_the_certificate_as_absent(&emulator, &row, &arn, &certificate_arn).await;
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

/// Taking its own certificate is what makes a CA's key one that can sign for others, and it
/// changes NOTHING about which refusal a read answers: the same ARN is the absence of a
/// certificate before the import and after it.
#[tokio::test]
async fn taking_its_own_certificate_leaves_the_refusal_a_read_answers_alone() {
    let emulator = Emulator::new();
    let case = "imported";
    let arn = created_ca(&emulator, "imported-root").await;
    wait_for_the_clock();

    let rows = unresolvable_certificate_arns(&arn);
    for (row, certificate_arn) in &rows {
        let row = format!("{case}: {row}, before its certificate");
        reports_the_certificate_as_absent(&emulator, &row, &arn, certificate_arn).await;
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
