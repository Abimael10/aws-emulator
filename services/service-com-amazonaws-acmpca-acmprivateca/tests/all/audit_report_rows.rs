// SPDX-License-Identifier: CC0-1.0

//! What one ROW of a CA's audit report states about a certificate.
//!
//! The rules under test.
//!
//! The report exists to be joined to the certificates a client holds, and the only field
//! that joins a row to a certificate the client parsed itself is the serial. A client reads
//! a serial off a certificate in the rendering X.509 tooling prints — pairs of lower-case
//! hexadecimal digits separated by colons — and that is the rendering a row states it in,
//! whatever the number is. The ARN beside it keeps the undelimited spelling an ARN is minted
//! with: the same number, spelled the way each field spells it.
//!
//! A row also states the template the certificate was issued UNDER, which for a request
//! that named none is the one `IssueCertificateRequest$TemplateArn` documents as the
//! default: a row states what the certificate carries, not what the request spelled out.
//!
//! A row records REVOCATION state only for a certificate that has been revoked: `revokedAt`
//! and `revocationReason` are stated, with the instant and the reason the revocation
//! carried, for a certificate `RevokeCertificate` was called for, and are absent from the row
//! of every certificate that was not — presence is what a consumer of the report reads.
//!
//! And the absence of a report is refused with the sentence Amazon Web Services Private CA
//! answers for that class, which is fixed text in full.
//!
//! The rows span what the rule generalises over: certificates whose serials are different
//! numbers, certificates that have been revoked — for different reasons — and ones that have
//! not, and a template named explicitly against one left out. The serial a row is checked
//! against is read off the certificate PEM the service hands out, never off anything this
//! suite chose.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_acmpca_acmprivateca::state::serial_octets;
use x509_cert::Certificate;

use crate::crl_bucket_resolution::Emulator;
use crate::private_ca_lifecycle::{blob, certificate_of, csr_pem, root_ca_request, string};
use crate::resource_policy::error_message;

/// The bucket every report in this suite is deposited in.
const BUCKET: &str = "acme-audit-report-rows";

/// What Amazon Web Services Private CA answers when the report an `AuditReportId` names
/// cannot be found: fixed text in full, written out here rather than read off the service's
/// own constant so a change to either has to be made in both.
const REPORT_NOT_FOUND_MESSAGE: &str = "Could not find specified Audit Report.";

/// The `TemplateArn` a certificate issued by a request that named none was issued under:
/// "If this parameter is not provided, Amazon Web Services Private CA defaults to the
/// `EndEntityCertificate/V1` template." (`IssueCertificateRequest$TemplateArn` docs.)
const DEFAULT_TEMPLATE_ARN: &str = "arn:aws:acm-pca:::template/EndEntityCertificate/V1";

/// The `TemplateArn` a row of this suite names explicitly.
const CLIENT_AUTH_TEMPLATE_ARN: &str =
    "arn:aws:acm-pca:::template/EndEntityClientAuthCertificate/V1";

/// The `TemplateArn` that produces a ROOT CA's own certificate, which is the certificate
/// the CA is activated with and the first use of its key a report states a row for.
const ROOT_CA_TEMPLATE_ARN: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// How long a poll for a due transition may take before the suite gives up. Generous: it
/// bounds a wait for a CONDITION, not a measurement of how long the service takes.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One certificate a report must state a row for.
struct Issued {
    /// The case every assertion about this certificate names.
    case: &'static str,
    /// The `TemplateArn` the issuance named, when it named one.
    template_arn: Option<&'static str>,
    /// The `TemplateArn` the row must state.
    stated_template_arn: &'static str,
    /// The `RevocationReason` the certificate is revoked with after it has been issued, or
    /// `None` for one that is left alone.
    revocation_reason: Option<&'static str>,
}

/// Every certificate the audited CA of this suite issues.
fn issued() -> Vec<Issued> {
    vec![
        Issued {
            case: "a certificate issued under the default template",
            template_arn: None,
            stated_template_arn: DEFAULT_TEMPLATE_ARN,
            revocation_reason: None,
        },
        Issued {
            case: "a certificate issued under a named template",
            template_arn: Some(CLIENT_AUTH_TEMPLATE_ARN),
            stated_template_arn: CLIENT_AUTH_TEMPLATE_ARN,
            revocation_reason: None,
        },
        Issued {
            case: "a revoked certificate",
            template_arn: Some(DEFAULT_TEMPLATE_ARN),
            stated_template_arn: DEFAULT_TEMPLATE_ARN,
            revocation_reason: Some("KEY_COMPROMISE"),
        },
        // A second revoked row, for another reason: what a row states is the reason the
        // revocation carried, not one reason the report is written with.
        Issued {
            case: "a certificate revoked because its subject ceased operation",
            template_arn: Some(CLIENT_AUTH_TEMPLATE_ARN),
            stated_template_arn: CLIENT_AUTH_TEMPLATE_ARN,
            revocation_reason: Some("CESSATION_OF_OPERATION"),
        },
    ]
}

/// The two members a row states only for a certificate that has been revoked.
const REVOCATION_MEMBERS: &[&str] = &["revokedAt", "revocationReason"];

/// The serial a certificate carries, as the octet pairs a client reads off it: the DER
/// integer's bytes, lower-case hexadecimal, colon-separated — what `openssl x509 -serial`
/// prints and what X.509 libraries render a serial as.
///
/// The leading zero byte a DER integer carries to stay positive is not part of the value, so
/// it is not part of the rendering either.
fn serial_of(certificate: &Certificate) -> String {
    let bytes = certificate.tbs_certificate.serial_number.as_bytes();
    let value: Vec<String> = bytes
        .iter()
        .skip_while(|byte| **byte == 0)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    value.join(":")
}

/// Poll `GetCertificate` until the issuance has completed, and answer the PEM it serves.
///
/// Issuance completes on the clock, so the suite looks again until the certificate is there
/// rather than waiting a span of its own.
///
/// `pub(crate)` so a sibling suite that has to let an issuance complete before it reads what
/// the CA has done polls through THIS helper rather than growing a second copy of it.
pub(crate) async fn poll_for_certificate(
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

/// Poll `DescribeCertificateAuthority` until the CA reports `status`.
///
/// A CA settles into its next status on the clock, so the suite looks again until the
/// condition holds rather than waiting a span of its own.
async fn poll_for_status(emulator: &Emulator, case: &str, arn: &str, status: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = emulator
            .ok(case, "DescribeCertificateAuthority", &json!({"CertificateAuthorityArn": arn}))
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

/// A ROOT CA driven all the way to ACTIVE the way the documented procedure drives one:
/// create it, read its certification request, sign that request under the root CA template,
/// and import what comes back. Answers the CA's ARN and its own certificate.
///
/// Every step is the service's own, so the CA's own certificate is one the CA ISSUED — a use
/// of its key, which is what an audit report states a row for.
async fn self_signed_active_ca(
    emulator: &Emulator,
    case: &str,
    common_name: &str,
) -> (String, String) {
    let created = emulator
        .ok(case, "CreateCertificateAuthority", &root_ca_request(common_name))
        .await;
    let arn = string(&created, "CertificateAuthorityArn", case);
    poll_for_status(emulator, case, &arn, "PENDING_CERTIFICATE").await;
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
                "TemplateArn": ROOT_CA_TEMPLATE_ARN,
                "Validity": {"Type": "YEARS", "Value": 10},
            }),
        )
        .await;
    let certificate_arn = string(&issued, "CertificateArn", case);
    let certificate = poll_for_certificate(emulator, case, &arn, &certificate_arn).await;
    emulator
        .ok(
            case,
            "ImportCertificateAuthorityCertificate",
            &json!({"CertificateAuthorityArn": arn, "Certificate": blob(&certificate)}),
        )
        .await;
    poll_for_status(emulator, case, &arn, "ACTIVE").await;
    (arn, certificate)
}

/// Every row of a JSON report, as the array the report object carries.
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

/// Every row's serial is the colon-separated octet-pair rendering of the certificate it
/// describes, so a client joins a serial it read off a certificate to exactly one row.
#[tokio::test]
async fn a_report_row_states_the_serial_in_the_octet_pairs_a_client_reads() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_bucket(BUCKET).await,
        200,
        "the bucket the reports are deposited in is created through S3"
    );
    let suite = "an audited CA";
    let (arn, ca_certificate) = self_signed_active_ca(&emulator, suite, "row-serial-root").await;

    // Issue every certificate the rows ask for, read each one back, and revoke the ones a
    // row says are revoked.
    let rows = issued();
    let mut certificates: Vec<(&Issued, String)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut body = json!({
            "CertificateAuthorityArn": arn,
            "Csr": blob(&csr_pem(&format!("row-leaf-{}", certificates.len()))),
            "SigningAlgorithm": "SHA256WITHRSA",
            "Validity": {"Type": "DAYS", "Value": 30},
        });
        if let Some(template_arn) = row.template_arn {
            body["TemplateArn"] = json!(template_arn);
        }
        let issued = emulator.ok(row.case, "IssueCertificate", &body).await;
        let certificate_arn = string(&issued, "CertificateArn", row.case);
        let pem = poll_for_certificate(&emulator, row.case, &arn, &certificate_arn).await;
        if let Some(reason) = row.revocation_reason {
            emulator
                .ok(
                    row.case,
                    "RevokeCertificate",
                    &json!({
                        "CertificateAuthorityArn": arn,
                        // The serial a client holds, in the rendering it read off the
                        // certificate: the same rendering the report states.
                        "CertificateSerial": serial_of(&certificate_of(&pem, row.case)),
                        "RevocationReason": reason,
                    }),
                )
                .await;
        }
        certificates.push((row, pem));
    }

    let report = report_rows(&emulator, suite, &arn).await;

    // The CA's own certificate is in the report too — the first use of its key — so the
    // report carries one row per certificate the CA has signed.
    assert_eq!(
        report.len(),
        certificates.len() + 1,
        "case {suite:?}: one row per certificate the CA signed, got {report:?}"
    );

    for (row, pem) in &certificates {
        let case = row.case;
        let certificate = certificate_of(pem, case);
        let serial = serial_of(&certificate);
        let matching: Vec<&Value> = report
            .iter()
            .filter(|entry| entry["serial"] == json!(serial))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "case {case:?}: expected exactly one row for serial {serial:?}, got {report:?}"
        );
        let entry = matching[0];

        // The row's serial is the octet-pair rendering, and the ARN beside it keeps the
        // undelimited spelling: the same number in each field's own spelling.
        let certificate_arn = string(entry, "certificateArn", case);
        let tail = certificate_arn
            .rsplit_once("/certificate/")
            .map(|(_, tail)| tail.to_string())
            .unwrap_or_default();
        assert_eq!(
            tail.trim_start_matches('0'),
            serial.replace(':', "").trim_start_matches('0'),
            "case {case:?}: the row's ARN names the same serial undelimited, got {entry:?}"
        );
        assert!(
            string(entry, "serial", case)
                .split(':')
                .all(|octet| octet.len() == 2
                    && octet
                        .chars()
                        .all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase())),
            "case {case:?}: every group of a row's serial is a lower-case hex octet, \
             got {entry:?}"
        );

        // The template the certificate was issued under, named or defaulted.
        assert_eq!(
            string(entry, "templateArn", case),
            row.stated_template_arn,
            "case {case:?}: the row states the template the certificate was issued under, \
             got {entry:?}"
        );

        // And the revocation, for the row that has one — a report records the revocation state
        // of a certificate that HAS one, so the two members are PRESENT only there.
        if let Some(reason) = row.revocation_reason {
            assert_eq!(
                string(entry, "revocationReason", case),
                reason,
                "case {case:?}: a revoked certificate's row carries its reason, got {entry:?}"
            );
            assert!(
                !string(entry, "revokedAt", case).is_empty(),
                "case {case:?}: a revoked certificate's row carries when, got {entry:?}"
            );
        } else {
            for member in REVOCATION_MEMBERS {
                assert!(
                    entry.get(member).is_none(),
                    "case {case:?}: a certificate that was not revoked states no {member:?}, \
                     got {entry:?}"
                );
            }
        }
    }

    // The CA's own certificate joins the same way, under the same rendering: a row for the
    // certificate the CA was activated with, stating the template it was issued under.
    let own_serial = serial_of(&certificate_of(&ca_certificate, suite));
    let own: Vec<&Value> = report
        .iter()
        .filter(|entry| entry["serial"] == json!(own_serial))
        .collect();
    assert_eq!(
        own.len(),
        1,
        "case {suite:?}: expected one row for the CA's own serial {own_serial:?}, \
         got {report:?}"
    );
    assert_eq!(
        string(own[0], "templateArn", suite),
        ROOT_CA_TEMPLATE_ARN,
        "case {suite:?}: the CA's own row states the template it was issued under, \
         got {:?}",
        own[0]
    );
    // The CA's own certificate was never revoked either, so its row states no revocation.
    for member in REVOCATION_MEMBERS {
        assert!(
            own[0].get(member).is_none(),
            "case {suite:?}: the CA's own row states no {member:?}, got {:?}",
            own[0]
        );
    }
}

/// The rendering itself, over the spellings a serial reaches this service in.
///
/// The rows are the ways one number is written: the undelimited hexadecimal a serial is
/// minted as, an odd number of digits (a leading nibble is the low half of a leading octet),
/// upper case, a value already written in octet pairs, and the shortest and longest serials
/// this service mints.
#[test]
fn a_serial_renders_as_the_octet_pairs_x509_tooling_prints() {
    // (the spelling, the rendering it must take)
    let rows: &[(&str, &str)] = &[
        ("c85cfc5c68f90d55ac9d3909a49aed4f", "c8:5c:fc:5c:68:f9:0d:55:ac:9d:39:09:a4:9a:ed:4f"),
        ("0a1b2c3d", "0a:1b:2c:3d"),
        ("a1b2c3d", "0a:1b:2c:3d"),
        ("C85CFC5C", "c8:5c:fc:5c"),
        ("c8:5c:fc:5c", "c8:5c:fc:5c"),
        ("00", "00"),
        ("1", "01"),
    ];
    for (spelling, rendering) in rows {
        let rendered = serial_octets(spelling);
        assert_eq!(
            rendered, *rendering,
            "case {spelling:?}: expected {rendering:?}, got {rendered:?}"
        );
    }
}

/// An `AuditReportId` that names no report of the CA is refused with the fixed sentence for
/// that class, whichever CA it is asked of.
#[tokio::test]
async fn an_audit_report_that_does_not_exist_is_refused_with_its_own_sentence() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_bucket(BUCKET).await,
        200,
        "the bucket the reports are deposited in is created through S3"
    );
    let (barren, _) =
        self_signed_active_ca(&emulator, "a CA with no report", "report-barren-root").await;
    let (holder, _) =
        self_signed_active_ca(&emulator, "a CA with a report", "report-holder-root").await;
    let held = string(
        &emulator
            .ok(
                "a CA with a report",
                "CreateCertificateAuthorityAuditReport",
                &json!({
                    "CertificateAuthorityArn": holder,
                    "S3BucketName": BUCKET,
                    "AuditReportResponseFormat": "JSON",
                }),
            )
            .await,
        "AuditReportId",
        "a CA with a report",
    );

    // (the case, the CA asked, the report id asked about)
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a well-formed id on a CA that has produced none",
            &barren,
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "a well-formed id on a CA that has produced one",
            &holder,
            "ffffffff-ffff-4fff-8fff-ffffffffffff".to_string(),
        ),
        (
            "another CA's report id",
            &barren,
            held.clone(),
        ),
    ];
    for (case, arn, report_id) in &rows {
        let refused = emulator
            .refused(
                case,
                "DescribeCertificateAuthorityAuditReport",
                &json!({"CertificateAuthorityArn": arn, "AuditReportId": report_id}),
                "ResourceNotFoundException",
            )
            .await;
        let message = error_message(&refused, case);
        assert_eq!(
            message, REPORT_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the sentence about the report, got {message:?}"
        );
    }

    // And the report the CA DID produce is still described, so the refusals above are about
    // the id and nothing else.
    emulator
        .ok(
            "the report that exists",
            "DescribeCertificateAuthorityAuditReport",
            &json!({"CertificateAuthorityArn": holder, "AuditReportId": held}),
        )
        .await;
}
