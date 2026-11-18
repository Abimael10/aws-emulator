// SPDX-License-Identifier: CC0-1.0

//! What a catalog row's `description` CARRIES, end to end over HTTP.
//!
//! RULE D — a report's `description` is the PUBLISHED description of the document: the
//! prose its publisher writes about it. So it is the document's own description rather
//! than a label naming it (a complete sentence about the document, never a restatement of
//! the row's `name`), it is as long as the publisher wrote it — one sentence for one
//! document and a paragraph for another, which is why nothing here bounds it below — it
//! satisfies the constraints the member's target declares, and every operation that
//! projects it — `ListReports`, `ListReportVersions` and `GetReportMetadata` at every
//! published version — reports that same one text, read from the one stored record.
//!
//! A description belongs to the DOCUMENT, so two documents of different publication
//! classes never share one. Within one class they can: the accessibility-conformance class
//! publishes one description about the template it reports on ("the relevant AWS product or
//! service"), which every report of the class carries — the recorded catalog entry of that
//! class is what shows it — while the product each one reports on is carried by its title.
//!
//! The rows below span the whole catalog, which differs along what the rule generalises
//! over: the report series it publishes, two categories, both `AcceptanceType` values,
//! reports publishing one, two, three and seven versions, and a published description that
//! is one sentence as well as ones that run to a paragraph. The texts the corpus records
//! are quoted from it — `report-073F0fUCdVVNKJrF`'s in
//! `issues/things-that-now-work/20261029T192742Z-artifact-read-plane-serves-an-incomplete-report-catalog.md`,
//! the accessibility class's and `report-5eevElBv6MyRmQsD`'s in the `ListReports` issue
//! that reported them — and the rest are the catalog's own, which this suite spells out
//! independently of `src/state.rs` so a row reseeded with a label fails here rather than
//! redefining what the suite asserts.

use serde_json::Value;

use crate::harness::{emulator, get_report_metadata, list_report_versions, list_reports, CATALOG};

/// `LongStringAttribute`'s published maximum, which `ReportSummary$description` and
/// `ReportDetail$description` both target: `@length(min: 1, max: 2048)`, `@pattern
/// ^[^<>]*$` (`smithy_query(query="shape",shape="LongStringAttribute")`).
const DESCRIPTION_MAX: usize = 2048;

/// The published description of every report of the accessibility-conformance class, as
/// the corpus records it for an entry of that class: one text about the template, which
/// every report of the class carries.
const ACCESSIBILITY_DESCRIPTION: &str =
    "This document is an Accessibility Conformance Report (ACR) based on the ITI VPAT® \
     and is intended to provide information on the accessibility of the relevant AWS \
     product or service. The VPAT references accessibility standards for information and \
     communications technology including: Section 508 (U.S.), EN 301 549 (EU), and \
     W3C/WAI WCAG.";

/// The published description of every report in the catalog, keyed by its `ReportId`.
///
/// Source for `report-073F0fUCdVVNKJrF` and `report-5eevElBv6MyRmQsD`: the recorded
/// `ListReports` body for those reports, as the issues named in this file's header quote
/// it. The accessibility-conformance rows carry that class's one published description,
/// also recorded. The others are the catalog's own published text (see `SEED` in
/// `src/state.rs`).
const PUBLISHED: &[(&str, &str)] = &[
    (
        "report-073F0fUCdVVNKJrF",
        "The AWS Outposts Information Security FAQ document is designed to help customer \
         teams responsible for information security evaluate the service against their \
         organizational requirements. The document is aligned to common questions relating \
         to people, process, and technology that this audience often ask. This document is \
         reviewed and updated regularly with the latest information.",
    ),
    (
        "report-0QjvdIs6Bsw4uPxb",
        ACCESSIBILITY_DESCRIPTION,
    ),
    (
        "report-0c1IXmudeXuTlHJJ",
        "The ISO/IEC 27001:2022 certificate records the certification of the AWS \
         information security management system by an accredited independent registrar. It \
         names the services and AWS Regions in the scope of the certification and the dates \
         of the certification cycle it was issued for. The certificate is reissued after \
         each surveillance and recertification audit.",
    ),
    (
        "report-5eevElBv6MyRmQsD",
        "Independent compliance assessment against the United Arab Emirates (UAE) \
         Information Assurance Regulation (IAR).",
    ),
    (
        "report-7g5j2DTVeNXWx7St",
        ACCESSIBILITY_DESCRIPTION,
    ),
    (
        "report-9pQ2mT8xLbV4sZ1c",
        "The AWS SOC 2 Type 2 report describes the AWS control environment and the results \
         of the independent auditor's tests of the controls relevant to the security, \
         availability and confidentiality trust services criteria. It covers the services \
         and AWS Regions listed in the report over the examination period stated on it. The \
         report is confidential and is released under the term the customer accepts before \
         downloading it.",
    ),
    (
        "report-Af3KdN7yRw2HqS5e",
        "The PCI DSS Attestation of Compliance records the assessment of the AWS \
         cardholder data environment against the Payment Card Industry Data Security \
         Standard by a qualified security assessor. It lists the services and AWS Regions \
         in scope and the requirements each one was assessed against. Customer teams use it \
         as the service provider attestation for their own PCI DSS assessments.",
    ),
    (
        "report-Bq7WmF3xTk9ZpL2a",
        "The AWS SOC 1 report describes the AWS control environment and the results of the \
         independent auditor's tests of the controls that are relevant to a customer's \
         internal control over financial reporting. It covers the services and AWS Regions \
         listed in the report over the examination period stated on it. Customer teams and \
         their auditors use it where AWS is a service organization in the scope of a \
         financial audit.",
    ),
    (
        "report-C4tYhR8nJd6VxQ3b",
        "The AWS SOC 3 report is the general use summary of the SOC 2 examination: it \
         carries the independent auditor's opinion on the AWS control environment without \
         the detailed tests of controls. It covers the services and AWS Regions listed in \
         the report over the examination period stated on it. Unlike the SOC 1 and SOC 2 \
         reports it may be shared freely.",
    ),
    (
        "report-D9kPzX2mLb7TnR4c",
        "The ISO/IEC 27017:2015 certificate records the certification of the AWS \
         information security controls for cloud services by an accredited independent \
         registrar. It names the services and AWS Regions in the scope of the certification \
         and the dates of the certification cycle it was issued for. The controls it covers \
         extend the ISO/IEC 27002 guidance to the provision and use of cloud services.",
    ),
    (
        "report-E2nVbK6yQw8MsZ5d",
        "The ISO/IEC 27018:2019 certificate records the certification of the AWS controls \
         for the protection of personally identifiable information processed in a public \
         cloud by an accredited independent registrar. It names the services and AWS \
         Regions in the scope of the certification and the dates of the certification cycle \
         it was issued for. Customer teams use it as evidence of the privacy controls a \
         public cloud processor is certified against.",
    ),
    (
        "report-F6sJdL9xRt3WqN7e",
        "The ISO/IEC 27701:2019 certificate records the certification of the AWS privacy \
         information management system by an accredited independent registrar. It names the \
         services and AWS Regions in the scope of the certification and the dates of the \
         certification cycle it was issued for. The certificate covers the extension of the \
         information security management system to the processing of personally \
         identifiable information.",
    ),
    (
        "report-G3mQfT7bXz5KpV8h",
        "The ISO 9001:2015 certificate records the certification of the AWS quality \
         management system by an accredited independent registrar. It names the services \
         and AWS Regions in the scope of the certification and the dates of the \
         certification cycle it was issued for. Customer teams in regulated industries use \
         it as evidence of the quality management AWS applies to the services in that \
         scope.",
    ),
    (
        "report-H8wRnP4cYk2JsL6m",
        "The FedRAMP Moderate customer package carries the security documentation for the \
         AWS Regions that hold a FedRAMP Moderate authorization, including the system \
         security plan summary and the assessment results an agency reviews before it \
         issues its own authority to operate. It names the services inside the \
         authorization boundary over the period stated on the package. Access is limited to \
         the customers the package may be released to.",
    ),
    (
        "report-J5zXkM9dVb3QtR7n",
        "The IRAP assessment report describes the assessment of the AWS Asia Pacific \
         Region in its scope against the controls of the Australian Government Information \
         Security Manual by an endorsed IRAP assessor. It lists the services assessed and \
         the control set each one was assessed against over the period stated on the \
         report. Australian government customers use it when accrediting their own systems.",
    ),
    (
        "report-K7bNqS2fWx8LmT4p",
        "The C5 attestation report records the attestation of the AWS controls against the \
         Cloud Computing Compliance Controls Catalogue that the German Federal Office for \
         Information Security publishes. It lists the services and AWS Regions in scope and \
         the basic and additional criteria each one was attested against over the period \
         stated on the report. Customer teams in Germany use it to meet their own cloud \
         assurance requirements.",
    ),
    (
        "report-L4dTvJ8hZn6RkQ9s",
        "The Consensus Assessments Initiative Questionnaire is the Cloud Security Alliance \
         questionnaire answered for AWS: it records the AWS response to each control \
         question of the Cloud Controls Matrix. It covers the services and AWS Regions \
         named in the questionnaire for the period stated on it. Customer teams use it to \
         compare the AWS control environment with the CSA control framework.",
    ),
    (
        "report-M2hXsL5bQw9TnF4j",
        ACCESSIBILITY_DESCRIPTION,
    ),
    (
        "report-N7kDvR3mYt8ZpB6c",
        ACCESSIBILITY_DESCRIPTION,
    ),
    (
        "report-P5qZmJ8hXc2VtL9d",
        ACCESSIBILITY_DESCRIPTION,
    ),
    (
        "report-Q8tBnW4fKz6RmS3h",
        ACCESSIBILITY_DESCRIPTION,
    ),
    (
        "report-R3vYkT9pLd5XnQ7b",
        ACCESSIBILITY_DESCRIPTION,
    ),
    (
        "report-S6mFqH2xVb8JtZ4n",
        ACCESSIBILITY_DESCRIPTION,
    ),
];

/// The published description this suite expects for `id`.
fn published(id: &str) -> &'static str {
    PUBLISHED
        .iter()
        .find(|(row, _)| *row == id)
        .map(|(_, description)| *description)
        .unwrap_or_else(|| {
            panic!(
                "case {id:?}: the catalog serves a report this suite names no published \
                 description for"
            )
        })
}

/// How many complete sentences `text` carries: a run of text closed by a full stop.
fn sentences(text: &str) -> usize {
    text.split_terminator(". ")
        .filter(|part| !part.trim().is_empty())
        .count()
}

/// The series the catalog reports for `id`, which is the publication CLASS a description
/// may be shared within.
fn series_of(id: &str) -> &'static str {
    CATALOG
        .iter()
        .find(|row| row.id == id)
        .map(|row| row.series)
        .unwrap_or_else(|| panic!("case {id:?}: the catalog holds no such report"))
}

/// The report the listing reports under `id`.
fn row<'a>(reports: &'a [Value], id: &str) -> &'a Value {
    reports
        .iter()
        .find(|entry| entry["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("case {id:?}: the catalog listing must report this report"))
}

#[tokio::test]
async fn every_catalog_row_reports_its_published_description() {
    let router = emulator();
    let listed = list_reports(&router, "").await;
    assert_eq!(
        listed.status, 200,
        "expected HTTP 200 from ListReports, got {} with {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.reports().len(),
        PUBLISHED.len(),
        "this suite names a published description for every catalog row, so the catalog and \
         the table must have the same length; got {} rows",
        listed.reports().len()
    );

    for expected in CATALOG {
        let case = expected.id;
        let text = published(case);
        let actual = row(listed.reports(), case)["description"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            actual, text,
            "case {case:?}: ListReports must report the published description, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_published_description_is_prose_about_the_document_not_a_label() {
    let router = emulator();
    let listed = list_reports(&router, "").await;

    for expected in CATALOG {
        let case = expected.id;
        let actual = row(listed.reports(), case)["description"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // The member's own constraints: `@length(min: 1, max: 2048)` and `@pattern
        // ^[^<>]*$`.
        assert!(
            (1..=DESCRIPTION_MAX).contains(&actual.chars().count())
                && !actual.contains('<')
                && !actual.contains('>'),
            "case {case:?}: a description must match ^[^<>]*$ within 1..=2048 characters, \
             got {actual:?}"
        );
        // Prose about the document: at least one complete sentence, closed by a full
        // stop. How MANY is the publisher's business — a recorded published description
        // runs to one sentence as well as to a paragraph.
        assert!(
            sentences(&actual) >= 1 && actual.ends_with('.'),
            "case {case:?}: expected a complete sentence of published description, got \
             {actual:?}"
        );
        // And it describes the document rather than restating the row's identity.
        assert!(
            actual != expected.name && actual != expected.series && actual != expected.category,
            "case {case:?}: a description must not restate the row's own name, series or \
             category, got {actual:?}"
        );
        // A description belongs to the document, so two documents share one only when
        // their publication CLASS publishes one text for the class (the
        // accessibility-conformance class publishes one description about the template).
        let shared_across_classes: Vec<&str> = PUBLISHED
            .iter()
            .filter(|(id, text)| {
                *id != case && *text == actual.as_str() && series_of(id) != expected.series
            })
            .map(|(id, _)| *id)
            .collect();
        assert!(
            shared_across_classes.is_empty(),
            "case {case:?}: a published description is shared only inside one class, also \
             found on {shared_across_classes:?}"
        );
    }
}

#[tokio::test]
async fn every_operation_projecting_a_description_reports_the_same_one() {
    let router = emulator();
    let listed = list_reports(&router, "").await;

    for expected in CATALOG {
        let text = published(expected.id);

        // Every published version of the report, as `ListReportVersions` reports them.
        let versions = list_report_versions(&router, &format!("?reportId={}", expected.id)).await;
        assert_eq!(
            versions.status, 200,
            "case {:?}: expected HTTP 200 from ListReportVersions, got {} with {:?}",
            expected.id, versions.status, versions.body
        );
        assert_eq!(
            versions.reports().len(),
            expected.versions.len(),
            "case {:?}: expected every published version, got {:?}",
            expected.id,
            versions.body
        );
        for (version, summary) in expected.versions.iter().zip(versions.reports()) {
            let case = format!("{} v{version}", expected.id);
            let actual = summary["description"].as_str().unwrap_or_default();
            assert_eq!(
                actual, text,
                "case {case:?}: ListReportVersions must report the published description, \
                 got {actual:?}"
            );

            // The single-report metadata read, at that same version.
            let metadata = get_report_metadata(
                &router,
                &format!("?reportId={}&reportVersion={version}", expected.id),
            )
            .await;
            assert_eq!(
                metadata.status, 200,
                "case {case:?}: expected HTTP 200 from GetReportMetadata, got {} with {:?}",
                metadata.status, metadata.body
            );
            let reported = metadata.body["reportDetails"]["description"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                reported, text,
                "case {case:?}: GetReportMetadata must report the published description, \
                 got {reported:?}"
            );
        }

        // And the catalog listing, which reports the latest version, agrees with both.
        let listed_text = row(listed.reports(), expected.id)["description"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            listed_text, text,
            "case {:?}: ListReports must report the same published description as the \
             per-version reads, got {listed_text:?}",
            expected.id
        );
    }
}
