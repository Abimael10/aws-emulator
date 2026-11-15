// SPDX-License-Identifier: CC0-1.0

//! Which issuances the `SHORT_LIVED_CERTIFICATE` usage mode's seven-day limit bounds.
//!
//! The rule under test: `UsageMode` says "whether the CA issues general-purpose
//! certificates ... or short-lived certificates ... Short-lived certificate validity is
//! limited to seven days" (`CreateCertificateAuthorityRequest$UsageMode`), so the seven-day
//! limit bounds the certificates such a CA issues to a SUBJECT and bounds no CA certificate
//! it is asked to sign; a CA certificate is bounded only by "the limit set on its parents in
//! the CA hierarchy" (`IssueCertificateRequest$Validity`), which a root CA has none of. A
//! request names which of the two it asks for with its `TemplateArn`: the `RootCACertificate`
//! and `SubordinateCACertificate` kinds define a CA certificate, every other published kind
//! defines a certificate for a subject, and a request naming no template is answered under
//! the default `EndEntityCertificate/V1`.
//!
//! The rows differ along the template kind (CA against subject, each kind of the published
//! `TemplateArn` catalog, with and without its passthrough and blank variants), along the
//! `ValidityPeriodType` the request states its window in, and along which side of the
//! seven-day limit that window falls. The template catalog is the one
//! `IssueCertificateRequest$TemplateArn` publishes ("For a list of TemplateArn values
//! supported by Amazon Web Services Private CA, see Understanding Certificate Templates"),
//! and it is exercised here on kinds beyond the root template the issue cited.
//!
//! Every call goes over the wire through the harness in [`super::private_ca_lifecycle`].

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::private_ca_lifecycle::{
    blob, certificate_of, csr_pem, root_ca_request, string, Emulator,
};
use crate::resource_policy::error_message;

/// How long a poll for a due transition may take before the suite gives up. Generous: it
/// bounds a wait for a CONDITION, not a measurement of how long the service takes.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The usage mode whose certificates "expire quickly"
/// (`CreateCertificateAuthorityRequest$UsageMode`).
const SHORT_LIVED: &str = "SHORT_LIVED_CERTIFICATE";

/// The template that produces a ROOT CA's own certificate.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// The same root CA certificate, taking the rest of its content from the API.
const ROOT_CA_PASSTHROUGH_TEMPLATE: &str =
    "arn:aws:acm-pca:::template/RootCACertificate_APIPassthrough/V1";

/// A blank root CA certificate whose name says it defines critical basic constraints.
const BLANK_ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/\
                                      BlankRootCACertificate_PathLen2_CriticalBasicConstraints_APIPassthrough/V1";

/// The seven days `UsageMode` limits a short-lived certificate to, in whole days.
const SHORT_LIVED_MAX_DAYS: u64 = 7;

/// A day, in seconds.
const SECONDS_PER_DAY: u64 = 86_400;

/// An `IssueCertificate` body for `ca_arn` over `csr`, under `template` (or under none at
/// all), valid for `validity`.
fn issuance(ca_arn: &str, csr: &str, template: Option<&str>, validity: &Value) -> Value {
    let mut body = json!({
        "CertificateAuthorityArn": ca_arn,
        "Csr": blob(csr),
        "SigningAlgorithm": "SHA256WITHRSA",
        "Validity": validity,
    });
    if let Some(template) = template {
        body["TemplateArn"] = json!(template);
    }
    body
}

/// A ROOT CA waiting for its certificate, with the certification request it published.
///
/// `usage_mode` is the `UsageMode` the CA is created with, or `None` for the default
/// (`GENERAL_PURPOSE`), which is how the control CA of this suite is created.
async fn pending_root(
    emulator: &Emulator,
    case: &str,
    common_name: &str,
    usage_mode: Option<&str>,
) -> (String, String) {
    let mut body = root_ca_request(common_name);
    if let Some(usage_mode) = usage_mode {
        body["UsageMode"] = json!(usage_mode);
    }
    let created = emulator
        .ok(case, "CreateCertificateAuthority", &body)
        .await;
    let arn = string(&created, "CertificateAuthorityArn", case);
    // Creation settles on the clock, so the suite looks again until the CA is waiting for its
    // certificate rather than waiting a span of its own.
    poll_for_status(emulator, case, &arn, "PENDING_CERTIFICATE").await;
    let reported = emulator
        .ok(
            case,
            "GetCertificateAuthorityCsr",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await;
    let csr = string(&reported, "Csr", case);
    (arn, csr)
}

/// A ROOT CA driven all the way to ACTIVE the way the documented procedure drives one:
/// create it, read its certification request, sign that request under the root CA template
/// for `validity`, and import what comes back.
async fn active_root(
    emulator: &Emulator,
    case: &str,
    common_name: &str,
    usage_mode: Option<&str>,
    validity: &Value,
) -> String {
    let (arn, csr) = pending_root(emulator, case, common_name, usage_mode).await;
    let issued = emulator
        .ok(
            case,
            "IssueCertificate",
            &issuance(&arn, &csr, Some(ROOT_CA_TEMPLATE), validity),
        )
        .await;
    let certificate_arn = string(&issued, "CertificateArn", case);
    let certificate = served_certificate(emulator, case, &arn, &certificate_arn).await;
    emulator
        .ok(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({
                "CertificateAuthorityArn": arn,
                "Certificate": blob(&certificate),
            }),
        )
        .await;
    poll_for_status(emulator, case, &arn, "ACTIVE").await;
    arn
}

/// Poll `DescribeCertificateAuthority` until the CA reports `status`.
///
/// A CA settles into its next status on the clock, so the suite looks again until the
/// condition holds rather than waiting a span of its own.
async fn poll_for_status(emulator: &Emulator, case: &str, arn: &str, status: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = emulator
            .ok(
                case,
                "DescribeCertificateAuthority",
                &json!({"CertificateAuthorityArn": arn}),
            )
            .await;
        let reported = string(&described["CertificateAuthority"], "Status", case);
        if reported == status {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the CA to report {status:?}, it still reports {reported:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Poll `GetCertificate` until the issuance `certificate_arn` names has completed, and answer
/// the PEM it serves.
///
/// Issuance completes on the clock, so the suite looks again until the certificate is there
/// rather than waiting a span of its own.
async fn served_certificate(
    emulator: &Emulator,
    case: &str,
    ca_arn: &str,
    certificate_arn: &str,
) -> String {
    let body = json!({
        "CertificateAuthorityArn": ca_arn,
        "CertificateArn": certificate_arn,
    });
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, parsed) = emulator.call("GetCertificate", &body).await;
        if status == 200 {
            return string(&parsed, "Certificate", case);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {certificate_arn} to be served, got {status} {parsed:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// How many whole days the validity period of the PEM certificate `certificate` spans.
fn window_days(certificate: &str, case: &str) -> u64 {
    let window = certificate_of(certificate, case).tbs_certificate.validity;
    let not_before = window.not_before.to_unix_duration().as_secs();
    let not_after = window.not_after.to_unix_duration().as_secs();
    assert!(
        not_after > not_before,
        "case {case:?}: expected a validity period that ends after it starts, got \
         {not_before}..{not_after}"
    );
    (not_after - not_before) / SECONDS_PER_DAY
}

/// The documented activation of a short-lived-mode root CA installs a certificate of its own
/// whose window runs far past seven days: the CA certificate is not one of the short-lived
/// certificates the mode is about.
#[tokio::test]
async fn a_short_lived_root_ca_signs_its_own_certificate_beyond_seven_days() {
    let emulator = Emulator::new();
    let (arn, csr) = pending_root(
        &emulator,
        "a pending short-lived root",
        "short-lived-root",
        Some(SHORT_LIVED),
    )
    .await;

    // (case, the CA-certificate template named, the validity requested, the whole days that
    // window spans). Each row is a window far past the seven-day limit, stated in a
    // different `ValidityPeriodType`; the three-year row is the one the recorded activation
    // asks for and the ten-year row is the one AWS's short-lived-CA walkthrough installs.
    // The variants of the CA-certificate templates are exercised on an ACTIVE CA in the
    // table below, because the certificate a CA signs before it holds one is its own root
    // certificate under the root CA certificate template (`super::root_ca_self_signing`).
    let rows: Vec<(&str, &str, Value, u64)> = vec![
        (
            "three years under the root CA template",
            ROOT_CA_TEMPLATE,
            json!({"Value": 3, "Type": "YEARS"}),
            1095,
        ),
        (
            "ten years under the root CA template",
            ROOT_CA_TEMPLATE,
            json!({"Value": 10, "Type": "YEARS"}),
            3650,
        ),
        (
            "one hundred and twenty months under the root CA template",
            ROOT_CA_TEMPLATE,
            json!({"Value": 120, "Type": "MONTHS"}),
            3650,
        ),
        (
            "three thousand six hundred and fifty days under the root CA template",
            ROOT_CA_TEMPLATE,
            json!({"Value": 3650, "Type": "DAYS"}),
            3650,
        ),
        (
            "twenty-four months under the root CA template",
            ROOT_CA_TEMPLATE,
            json!({"Value": 24, "Type": "MONTHS"}),
            730,
        ),
    ];

    // Issue every row first, then read each certificate back, rather than polling between
    // every pair of calls.
    let mut issued: Vec<(&str, u64, String)> = Vec::new();
    for (case, template, validity, days) in &rows {
        let answered = emulator
            .ok(
                case,
                "IssueCertificate",
                &issuance(&arn, &csr, Some(template), validity),
            )
            .await;
        let certificate_arn = string(&answered, "CertificateArn", case);
        assert!(
            certificate_arn.starts_with(&format!("{arn}/certificate/")),
            "case {case:?}: expected a certificate ARN of {arn:?}, got {certificate_arn:?}"
        );
        issued.push((case, *days, certificate_arn));
    }

    for (case, days, certificate_arn) in &issued {
        let certificate = served_certificate(&emulator, case, &arn, certificate_arn).await;
        let spanned = window_days(&certificate, case);
        assert!(
            spanned > SHORT_LIVED_MAX_DAYS,
            "case {case:?}: expected the CA's own certificate to run past the \
             {SHORT_LIVED_MAX_DAYS}-day short-lived limit, got {spanned} days"
        );
        // A month and a year are calendar spans, so the window lands on the requested day
        // give or take the leap days inside it.
        assert!(
            (*days..days + 5).contains(&spanned),
            "case {case:?}: expected a validity period of about {days} days, got {spanned}"
        );
    }

    // And the first of them activates the CA, which is what the documented procedure asks
    // for: the certificate the CA signed for itself is the one the import holds.
    let case = "the activation";
    let (_, _, certificate_arn) = &issued[0];
    let certificate = served_certificate(&emulator, case, &arn, certificate_arn).await;
    emulator
        .ok(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({
                "CertificateAuthorityArn": arn,
                "Certificate": blob(&certificate),
            }),
        )
        .await;
    let authority = emulator
        .ok(
            case,
            "DescribeCertificateAuthority",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await["CertificateAuthority"]
        .clone();
    let status = string(&authority, "Status", case);
    assert_eq!(
        status, "ACTIVE",
        "case {case:?}: expected the imported certificate to activate the short-lived CA, \
         got {status:?}"
    );
    let usage_mode = string(&authority, "UsageMode", case);
    assert_eq!(
        usage_mode, SHORT_LIVED,
        "case {case:?}: expected the activated CA to keep its usage mode, got {usage_mode:?}"
    );
    let reported_not_after = authority["NotAfter"].as_f64().unwrap_or_default() as u64;
    let own_not_after = certificate_of(&certificate, case)
        .tbs_certificate
        .validity
        .not_after
        .to_unix_duration()
        .as_secs();
    assert_eq!(
        reported_not_after, own_not_after,
        "case {case:?}: expected the CA to report the NotAfter of the certificate it took \
         ({own_not_after}), got {reported_not_after}"
    );
}

/// An ACTIVE short-lived-mode CA caps what it issues to a SUBJECT at seven days and caps no
/// CA certificate it signs.
#[tokio::test]
async fn a_short_lived_ca_caps_only_the_certificates_it_issues_to_a_subject() {
    let emulator = Emulator::new();
    let case = "an active short-lived CA";
    // Thirty years, so every window a row below asks for sits inside the CA's own and no row
    // is refused for the hierarchy limit instead (`IssueCertificateRequest$Validity`).
    let arn = active_root(
        &emulator,
        case,
        "short-lived-issuer",
        Some(SHORT_LIVED),
        &json!({"Value": 30, "Type": "YEARS"}),
    )
    .await;

    let leaf = csr_pem("a-subject-request");

    // (case, the template named, the validity requested, whether the CA answers it)
    let rows: Vec<(&str, Option<&str>, Value, bool)> = vec![
        // The CA certificates: no seven-day limit, whatever their window.
        (
            "a subordinate CA certificate of depth zero for five years",
            Some("arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen0/V1"),
            json!({"Value": 5, "Type": "YEARS"}),
            true,
        ),
        (
            "a subordinate CA certificate of depth two for a year, passing the API through",
            Some("arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen2_APIPassthrough/V1"),
            json!({"Value": 365, "Type": "DAYS"}),
            true,
        ),
        (
            "a subordinate CA certificate of depth three for sixty months, passing the CSR \
             through",
            Some(
                "arn:aws:acm-pca:::template/\
                 SubordinateCACertificate_PathLen3_APICSRPassthrough/V1",
            ),
            json!({"Value": 60, "Type": "MONTHS"}),
            true,
        ),
        (
            "a blank subordinate CA certificate for ten years",
            Some(
                "arn:aws:acm-pca:::template/\
                 BlankSubordinateCACertificate_PathLen0_CriticalBasicConstraints_APIPassthrough/V1",
            ),
            json!({"Value": 10, "Type": "YEARS"}),
            true,
        ),
        (
            "a root CA certificate for ten years, passing the API through",
            Some(ROOT_CA_PASSTHROUGH_TEMPLATE),
            json!({"Value": 3650, "Type": "DAYS"}),
            true,
        ),
        (
            "a blank root CA certificate for two years",
            Some(BLANK_ROOT_CA_TEMPLATE),
            json!({"Value": 2, "Type": "YEARS"}),
            true,
        ),
        // The certificates issued to a subject: capped at seven days, whichever kind names
        // them and whichever way the window is stated.
        (
            "an end-entity certificate for eight days",
            Some("arn:aws:acm-pca:::template/EndEntityCertificate/V1"),
            json!({"Value": 8, "Type": "DAYS"}),
            false,
        ),
        (
            "a client-authentication certificate for thirty days",
            Some("arn:aws:acm-pca:::template/EndEntityClientAuthCertificate/V1"),
            json!({"Value": 30, "Type": "DAYS"}),
            false,
        ),
        (
            "a server-authentication certificate for a month, passing the API through",
            Some(
                "arn:aws:acm-pca:::template/EndEntityServerAuthCertificate_APIPassthrough/V1",
            ),
            json!({"Value": 1, "Type": "MONTHS"}),
            false,
        ),
        (
            "a code-signing certificate for a year",
            Some("arn:aws:acm-pca:::template/CodeSigningCertificate/V1"),
            json!({"Value": 1, "Type": "YEARS"}),
            false,
        ),
        (
            "an OCSP-signing certificate for ten years",
            Some("arn:aws:acm-pca:::template/OCSPSigningCertificate/V1"),
            json!({"Value": 3650, "Type": "DAYS"}),
            false,
        ),
        (
            "a blank end-entity certificate for a fortnight",
            Some("arn:aws:acm-pca:::template/BlankEndEntityCertificate_APIPassthrough/V1"),
            json!({"Value": 14, "Type": "DAYS"}),
            false,
        ),
        (
            "no template at all for eight days",
            None,
            json!({"Value": 8, "Type": "DAYS"}),
            false,
        ),
        (
            "a template kind outside the published ones for eight days",
            Some("arn:aws:acm-pca:::template/NoSuchCertificateKind/V1"),
            json!({"Value": 8, "Type": "DAYS"}),
            false,
        ),
        // And inside the seven days, the same subject certificates are answered.
        (
            "an end-entity certificate for seven days",
            Some("arn:aws:acm-pca:::template/EndEntityCertificate/V1"),
            json!({"Value": 7, "Type": "DAYS"}),
            true,
        ),
        (
            "a server-authentication certificate for a day",
            Some("arn:aws:acm-pca:::template/EndEntityServerAuthCertificate/V1"),
            json!({"Value": 1, "Type": "DAYS"}),
            true,
        ),
        (
            "no template at all for seven days",
            None,
            json!({"Value": 7, "Type": "DAYS"}),
            true,
        ),
        (
            "a blank end-entity certificate for seven days",
            Some("arn:aws:acm-pca:::template/BlankEndEntityCertificate_APIPassthrough/V1"),
            json!({"Value": 7, "Type": "DAYS"}),
            true,
        ),
    ];

    for (case, template, validity, answered) in &rows {
        let body = issuance(&arn, &leaf, *template, validity);
        if *answered {
            let issued = emulator.ok(case, "IssueCertificate", &body).await;
            let certificate_arn = string(&issued, "CertificateArn", case);
            assert!(
                certificate_arn.starts_with(&format!("{arn}/certificate/")),
                "case {case:?}: expected a certificate ARN of {arn:?}, got {certificate_arn:?}"
            );
        } else {
            let refused = emulator
                .refused(case, "IssueCertificate", &body, "InvalidArgsException")
                .await;
            let message = error_message(&refused, case);
            assert!(
                message.contains(SHORT_LIVED),
                "case {case:?}: expected the refusal to name the usage mode that limits the \
                 window, got {message:?}"
            );
        }
    }

    // The control: the limit belongs to the usage mode, so a GENERAL_PURPOSE CA answers the
    // very window its short-lived counterpart refused.
    let case = "a general-purpose CA";
    let general = active_root(
        &emulator,
        case,
        "general-purpose-issuer",
        None,
        &json!({"Value": 10, "Type": "YEARS"}),
    )
    .await;
    let issued = emulator
        .ok(
            case,
            "IssueCertificate",
            &issuance(
                &general,
                &leaf,
                Some("arn:aws:acm-pca:::template/EndEntityCertificate/V1"),
                &json!({"Value": 30, "Type": "DAYS"}),
            ),
        )
        .await;
    let certificate_arn = string(&issued, "CertificateArn", case);
    assert!(
        certificate_arn.starts_with(&format!("{general}/certificate/")),
        "case {case:?}: expected a certificate ARN of {general:?}, got {certificate_arn:?}"
    );
}
