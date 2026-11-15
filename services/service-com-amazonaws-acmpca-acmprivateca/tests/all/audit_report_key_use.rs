// SPDX-License-Identifier: CC0-1.0

//! Which uses of a private CA's key an audit report states a row for.
//!
//! THE RULE: "Creates an audit report that lists every time that your CA private key is used
//! to issue a certificate. The `IssueCertificate` and `RevokeCertificate` actions use the
//! private key." (`CreateCertificateAuthorityAuditReport` docs.) The key is used BY the
//! issuance, so every certificate the CA has issued is a row of the next report it produces,
//! as of the instant that report is produced. Whether a certificate has become retrievable
//! through `GetCertificate` is a separate, retrieval-side question and decides nothing about
//! the report: a certificate issued moments before the report is in it.
//!
//! The rows span what the rule generalises over: the CA's own certificate and the
//! certificates it issues to a subject, under a named template, under none at all and under a
//! CA template, each with a window of its own — and a second report produced after the first,
//! which states the rows of the first plus whatever the CA signed in between.
//!
//! The reports are deposited in the emulator's OWN object storage, so what is asserted is the
//! object a client downloads.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::crl_bucket_resolution::Emulator;
use crate::private_ca_lifecycle::{blob, csr_pem, root_ca_request, string};

/// The bucket every report of this suite is deposited in.
const BUCKET: &str = "acme-audit-report-key-use";

/// The template that produces a ROOT CA's own certificate.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// The `TemplateArn` a certificate issued by a request that named none was issued under: "If
/// this parameter is not provided, Amazon Web Services Private CA defaults to the
/// `EndEntityCertificate/V1` template." (`IssueCertificateRequest$TemplateArn` docs.)
const DEFAULT_TEMPLATE_ARN: &str = "arn:aws:acm-pca:::template/EndEntityCertificate/V1";

/// How long a poll for a due transition may take before the suite gives up. Generous: it
/// bounds a wait for a CONDITION, not a measurement of how long the service takes.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Poll `DescribeCertificateAuthority` until the CA reports `status`.
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

/// A ROOT CA driven to ACTIVE the way the documented procedure drives one: created, its
/// certification request read, that request signed by the CA ITSELF under the root CA
/// template, and the answer imported.
///
/// Every step is the service's own, so the CA's own certificate is one the CA ISSUED — a use
/// of its key, which is what an audit report states a row for.
async fn active_ca(emulator: &Emulator, case: &str, common_name: &str) -> String {
    let created = emulator
        .ok(case, "CreateCertificateAuthority", &root_ca_request(common_name))
        .await;
    let arn = string(&created, "CertificateAuthorityArn", case);
    poll_for_status(emulator, case, &arn, "PENDING_CERTIFICATE").await;
    let reported = emulator
        .ok(
            case,
            "GetCertificateAuthorityCsr",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await;
    let issued = emulator
        .ok(
            case,
            "IssueCertificate",
            &json!({
                "CertificateAuthorityArn": arn,
                "Csr": blob(&string(&reported, "Csr", case)),
                "SigningAlgorithm": "SHA256WITHRSA",
                "TemplateArn": ROOT_CA_TEMPLATE,
                "Validity": {"Type": "YEARS", "Value": 10},
            }),
        )
        .await;
    let certificate_arn = string(&issued, "CertificateArn", case);
    let certificate = served_certificate(emulator, case, &arn, &certificate_arn).await;
    emulator
        .ok(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({"CertificateAuthorityArn": arn, "Certificate": blob(&certificate)}),
        )
        .await;
    poll_for_status(emulator, case, &arn, "ACTIVE").await;
    arn
}

/// Produce a report and answer the rows of the object it was deposited as.
async fn report_rows(emulator: &Emulator, case: &str, arn: &str) -> Vec<Value> {
    let created = emulator
        .ok(
            case,
            "CreateCertificateAuthorityAuditReport",
            &json!({
                "CertificateAuthorityArn": arn,
                "S3BucketName": BUCKET,
                "AuditReportResponseFormat": "JSON",
            }),
        )
        .await;
    let key = string(&created, "S3Key", case);
    let (status, body) = emulator
        .s3("GET", &format!("{BUCKET}/{key}"), &[], Vec::new())
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the report to be deposited at {key:?}, got {status}"
    );
    let text = String::from_utf8(body).expect("the report is text");
    let parsed: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("case {case:?}: the report is JSON: {error}"));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a JSON report is an array: {parsed:?}"))
        .clone()
}

/// The row of `rows` whose `certificateArn` is `certificate_arn`, or a panic naming the case.
fn row_for<'rows>(rows: &'rows [Value], certificate_arn: &str, case: &str) -> &'rows Value {
    let matching: Vec<&Value> = rows
        .iter()
        .filter(|row| row["certificateArn"] == json!(certificate_arn))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one row for {certificate_arn:?}, got {rows:?}"
    );
    matching[0]
}

/// THE RULE: every certificate the CA has issued is in the report it produces next, and it is
/// there from the issuance itself — the report is asked for straight after the issuances, with
/// nothing waiting on the certificates becoming retrievable.
#[tokio::test]
async fn a_report_lists_every_certificate_the_ca_has_issued() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_bucket(BUCKET).await,
        200,
        "the bucket the reports are deposited in is created through S3"
    );
    let suite = "an audited CA";
    let arn = active_ca(&emulator, suite, "audited-root").await;

    // (case, the template the issuance names, the template the row must state, the validity)
    let rows: Vec<(&str, Option<&str>, &str, Value)> = vec![
        (
            "a certificate issued under no template at all",
            None,
            DEFAULT_TEMPLATE_ARN,
            json!({"Type": "DAYS", "Value": 30}),
        ),
        (
            "a certificate issued under a named end-entity template",
            Some("arn:aws:acm-pca:::template/EndEntityClientAuthCertificate/V1"),
            "arn:aws:acm-pca:::template/EndEntityClientAuthCertificate/V1",
            json!({"Type": "MONTHS", "Value": 2}),
        ),
        (
            "a code-signing certificate",
            Some("arn:aws:acm-pca:::template/CodeSigningCertificate/V1"),
            "arn:aws:acm-pca:::template/CodeSigningCertificate/V1",
            json!({"Type": "DAYS", "Value": 1}),
        ),
        (
            "a subordinate CA certificate",
            Some("arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen0/V1"),
            "arn:aws:acm-pca:::template/SubordinateCACertificate_PathLen0/V1",
            json!({"Type": "YEARS", "Value": 1}),
        ),
    ];

    // Every issuance first, then ONE report over all of them: no step in between waits for a
    // certificate to become retrievable, which is what the rule is about.
    let mut issued: Vec<(&str, &str, String)> = Vec::new();
    for (case, template, stated_template, validity) in &rows {
        let mut body = json!({
            "CertificateAuthorityArn": arn,
            "Csr": blob(&csr_pem(&format!("audited-leaf-{}", issued.len()))),
            "SigningAlgorithm": "SHA256WITHRSA",
            "Validity": validity,
        });
        if let Some(template) = template {
            body["TemplateArn"] = json!(template);
        }
        let answered = emulator.ok(case, "IssueCertificate", &body).await;
        issued.push((
            case,
            stated_template,
            string(&answered, "CertificateArn", case),
        ));
    }

    let report = report_rows(&emulator, suite, &arn).await;
    for (case, stated_template, certificate_arn) in &issued {
        let row = row_for(&report, certificate_arn, case);
        assert_eq!(
            string(row, "templateArn", case),
            *stated_template,
            "case {case:?}: the row states the template the certificate was issued under, got \
             {row:?}"
        );
        assert!(
            !string(row, "serial", case).is_empty()
                && !string(row, "notAfter", case).is_empty()
                && !string(row, "issuedAt", case).is_empty(),
            "case {case:?}: a row states the certificate's serial, window and issuance, got \
             {row:?}"
        );
    }
    // One row per certificate the CA signed: the issuances above, and the CA's own
    // certificate, which was signed by the CA itself and is a use of its key too.
    assert_eq!(
        report.len(),
        issued.len() + 1,
        "case {suite:?}: expected one row per certificate the CA signed, got {report:?}"
    );
    assert_eq!(
        report
            .iter()
            .filter(|row| row["templateArn"] == json!(ROOT_CA_TEMPLATE))
            .count(),
        1,
        "case {suite:?}: expected the CA's own certificate to be a row of the report, got \
         {report:?}"
    );

    // A second report states the rows of the first plus whatever the CA signed in between —
    // again with nothing waiting on the new certificate.
    let case = "a certificate issued after the first report";
    let answered = emulator
        .ok(
            case,
            "IssueCertificate",
            &json!({
                "CertificateAuthorityArn": arn,
                "Csr": blob(&csr_pem("audited-leaf-after-the-report")),
                "SigningAlgorithm": "SHA256WITHRSA",
                "Validity": {"Type": "DAYS", "Value": 7},
            }),
        )
        .await;
    let certificate_arn = string(&answered, "CertificateArn", case);
    let second = report_rows(&emulator, case, &arn).await;
    assert_eq!(
        second.len(),
        report.len() + 1,
        "case {case:?}: expected the later report to add exactly the new issuance, got {second:?}"
    );
    let row = row_for(&second, &certificate_arn, case);
    assert_eq!(
        string(row, "templateArn", case),
        DEFAULT_TEMPLATE_ARN,
        "case {case:?}: the row states the template the certificate was issued under, got {row:?}"
    );
}
