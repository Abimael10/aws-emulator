// SPDX-License-Identifier: CC0-1.0

//! The accessibility-conformance class of the published report catalog: the Accessibility
//! Conformance Report (the Voluntary Product Accessibility Template) AWS publishes for a
//! product it has evaluated.
//!
//! This class lives in its own file because of how it is SHAPED. The rest of the catalog
//! ([`super::SEED`]) is one row per compliance programme — one certificate, one
//! attestation, one questionnaire — while an accessibility-conformance report is published
//! PER PRODUCT, so the class is a set of rows that grows with the products AWS evaluates
//! and shares one series, one category and one acceptance type between all of them. Those
//! three are therefore stated once here rather than repeated on every row.
//!
//! Catalog membership is not something the model bounds: AWS Artifact has no
//! `CreateReport`, so what the catalog holds is the published inventory, and this class is
//! part of it. Two rows below carry the ids the corpus discloses for an
//! accessibility-conformance report — `report-7g5j2DTVeNXWx7St` at one published version
//! and `report-0QjvdIs6Bsw4uPxb` at two — which are AWS's own, as a recorded `ListReports`
//! body shows them; the remaining rows carry ids this catalog assigns (they satisfy
//! `ReportId`'s `^report-[a-zA-Z0-9]{16}$` and are not a claim that AWS issued them), as
//! [`super::SEED`]'s own doc comment explains for the rows there.
//!
//! Every value satisfies its member's modelled constraints: `name`, `series`, `category`
//! and `productName` match `ShortStringAttribute`'s `^[a-zA-Z0-9_\-\s]*$` within 1..=256
//! characters — which is why a report's name spells the template out rather than
//! parenthesising it — and each `description` matches `LongStringAttribute`'s `^[^<>]*$`
//! within 1..=2048.

use super::*;

/// The series AWS publishes every accessibility-conformance report under, as the recorded
/// catalog entries for this class name it.
const ACCESSIBILITY_SERIES: &str = "ACR";

/// The category this class is published in — the same one the certificates and
/// attestations carry: an accessibility-conformance report is an evaluation of a product
/// against published criteria.
const ACCESSIBILITY_CATEGORY: &str = "Certifications and Attestations";

/// `AcceptanceType$PASSTHROUGH`: an accessibility-conformance report is published for
/// everyone, so no click-through acceptance of its term is required of a downloader. The
/// recorded entries of this class carry this value.
const ACCESSIBILITY_ACCEPTANCE: &str = "PASSTHROUGH";

/// The accessibility-conformance reports the catalog publishes, one per evaluated product.
///
/// The rows differ along what the report surface generalises over: the product evaluated,
/// the number of published versions (one, two and three) and the reporting windows those
/// versions cover. Each `description` is the published description of that document — the
/// prose a publisher writes about it — and names the product's own surfaces, so no two
/// documents share one description.
///
/// A version's `published_at` opens the window it covers rather than closing it: an
/// accessibility conformance report states the conformance of a product AS EVALUATED, and
/// is published when that evaluation completes and then stands for the period that follows
/// — the same way [`super::SEED`]'s certificates carry the start of the cycle they were
/// issued for, and unlike an examination report, which is published once the period it
/// reports on has elapsed.
pub(super) const ACCESSIBILITY_SEED: &[SeedReport] = &[
    SeedReport {
        id: "report-0QjvdIs6Bsw4uPxb",
        name: "Accessibility Conformance Report VPAT - Amazon Connect",
        description: "The Accessibility Conformance Report for Amazon Connect records how \
                      the product's user interfaces meet the accessibility criteria of \
                      Section 508 of the US Rehabilitation Act, EN 301 549 and the Web \
                      Content Accessibility Guidelines, reported in the Voluntary Product \
                      Accessibility Template that the Information Technology Industry \
                      Council publishes. The evaluation covers the agent workspace, the \
                      contact control panel and the administration website. Each criterion \
                      is reported as supported, partially supported or not applicable, with \
                      the remarks the evaluator recorded for it. Customer teams use it to \
                      assess the product against their own accessibility obligations before \
                      they roll it out to their users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-Amazon-Connect.pdf",
        term_object: "Accessibility-Conformance-Report-Amazon-Connect-Term.txt",
        versions: &[
            SeedVersion {
                version: 1,
                period_start: T2023JAN,
                period_end: T2024JAN,
                published_at: T2023JAN,
                sequence_number: 1,
            },
            SeedVersion {
                version: 2,
                period_start: T2024JAN,
                period_end: T2025JAN,
                published_at: T2024JAN,
                sequence_number: 2,
            },
        ],
    },
    SeedReport {
        id: "report-7g5j2DTVeNXWx7St",
        name: "Accessibility Conformance Report VPAT - AWS Management Console",
        description: "The Accessibility Conformance Report for the AWS Management Console \
                      records how the console's user interfaces meet the accessibility \
                      criteria of Section 508 of the US Rehabilitation Act, EN 301 549 and \
                      the Web Content Accessibility Guidelines, reported in the Voluntary \
                      Product Accessibility Template that the Information Technology \
                      Industry Council publishes. The evaluation covers the console's \
                      navigation, the service consoles reached from it and the \
                      documentation it links to. Each criterion is reported as supported, \
                      partially supported or not applicable, with the remarks the evaluator \
                      recorded for it. Customer teams use it to assess the console against \
                      their own accessibility obligations before they roll it out to their \
                      users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-AWS-Management-Console.pdf",
        term_object: "Accessibility-Conformance-Report-AWS-Management-Console-Term.txt",
        versions: &[SeedVersion {
            version: 1,
            period_start: T2024JAN,
            period_end: T2025JAN,
            published_at: T2025JAN,
            sequence_number: 1,
        }],
    },
    SeedReport {
        id: "report-M2hXsL5bQw9TnF4j",
        name: "Accessibility Conformance Report VPAT - Amazon WorkSpaces",
        description: "The Accessibility Conformance Report for Amazon WorkSpaces records \
                      how the product's user interfaces meet the accessibility criteria of \
                      Section 508 of the US Rehabilitation Act, EN 301 549 and the Web \
                      Content Accessibility Guidelines, reported in the Voluntary Product \
                      Accessibility Template that the Information Technology Industry \
                      Council publishes. The evaluation covers the desktop clients, the web \
                      access client and the administration website. Each criterion is \
                      reported as supported, partially supported or not applicable, with \
                      the remarks the evaluator recorded for it. Customer teams use it to \
                      assess the product against their own accessibility obligations before \
                      they roll it out to their users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-Amazon-WorkSpaces.pdf",
        term_object: "Accessibility-Conformance-Report-Amazon-WorkSpaces-Term.txt",
        versions: &[
            SeedVersion {
                version: 1,
                period_start: T2023JUL,
                period_end: T2024JUL,
                published_at: T2023JUL,
                sequence_number: 1,
            },
            SeedVersion {
                version: 2,
                period_start: T2024JUL,
                period_end: T2025JUL,
                published_at: T2024JUL,
                sequence_number: 2,
            },
        ],
    },
    SeedReport {
        id: "report-N7kDvR3mYt8ZpB6c",
        name: "Accessibility Conformance Report VPAT - Amazon QuickSight",
        description: "The Accessibility Conformance Report for Amazon QuickSight records \
                      how the product's user interfaces meet the accessibility criteria of \
                      Section 508 of the US Rehabilitation Act, EN 301 549 and the Web \
                      Content Accessibility Guidelines, reported in the Voluntary Product \
                      Accessibility Template that the Information Technology Industry \
                      Council publishes. The evaluation covers dashboard authoring, the \
                      reader experience and the embedded analytics surfaces. Each criterion \
                      is reported as supported, partially supported or not applicable, with \
                      the remarks the evaluator recorded for it. Customer teams use it to \
                      assess the product against their own accessibility obligations before \
                      they roll it out to their users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-Amazon-QuickSight.pdf",
        term_object: "Accessibility-Conformance-Report-Amazon-QuickSight-Term.txt",
        versions: &[SeedVersion {
            version: 1,
            period_start: T2024JAN,
            period_end: T2025JAN,
            published_at: T2024JAN,
            sequence_number: 1,
        }],
    },
    SeedReport {
        id: "report-P5qZmJ8hXc2VtL9d",
        name: "Accessibility Conformance Report VPAT - Amazon Chime SDK",
        description: "The Accessibility Conformance Report for the Amazon Chime SDK records \
                      how the product's user interfaces meet the accessibility criteria of \
                      Section 508 of the US Rehabilitation Act, EN 301 549 and the Web \
                      Content Accessibility Guidelines, reported in the Voluntary Product \
                      Accessibility Template that the Information Technology Industry \
                      Council publishes. The evaluation covers the meeting client, the \
                      messaging surfaces and the call controls. Each criterion is reported \
                      as supported, partially supported or not applicable, with the remarks \
                      the evaluator recorded for it. Customer teams use it to assess the \
                      product against their own accessibility obligations before they roll \
                      it out to their users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-Amazon-Chime-SDK.pdf",
        term_object: "Accessibility-Conformance-Report-Amazon-Chime-SDK-Term.txt",
        versions: &[
            SeedVersion {
                version: 1,
                period_start: T2023JAN,
                period_end: T2024JAN,
                published_at: T2023JAN,
                sequence_number: 1,
            },
            SeedVersion {
                version: 2,
                period_start: T2024JAN,
                period_end: T2025JAN,
                published_at: T2024JAN,
                sequence_number: 2,
            },
            SeedVersion {
                version: 3,
                period_start: T2025JAN,
                period_end: T2026JAN,
                published_at: T2025JAN,
                sequence_number: 3,
            },
        ],
    },
    SeedReport {
        id: "report-Q8tBnW4fKz6RmS3h",
        name: "Accessibility Conformance Report VPAT - AWS CloudShell",
        description: "The Accessibility Conformance Report for AWS CloudShell records how \
                      the product's user interfaces meet the accessibility criteria of \
                      Section 508 of the US Rehabilitation Act, EN 301 549 and the Web \
                      Content Accessibility Guidelines, reported in the Voluntary Product \
                      Accessibility Template that the Information Technology Industry \
                      Council publishes. The evaluation covers the terminal surface, its \
                      keyboard interaction and the file transfer controls. Each criterion is \
                      reported as supported, partially supported or not applicable, with \
                      the remarks the evaluator recorded for it. Customer teams use it to \
                      assess the product against their own accessibility obligations before \
                      they roll it out to their users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-AWS-CloudShell.pdf",
        term_object: "Accessibility-Conformance-Report-AWS-CloudShell-Term.txt",
        versions: &[SeedVersion {
            version: 1,
            period_start: T2024JUL,
            period_end: T2025JUL,
            published_at: T2024JUL,
            sequence_number: 1,
        }],
    },
    SeedReport {
        id: "report-R3vYkT9pLd5XnQ7b",
        name: "Accessibility Conformance Report VPAT - Amazon SageMaker Studio",
        description: "The Accessibility Conformance Report for Amazon SageMaker Studio \
                      records how the product's user interfaces meet the accessibility \
                      criteria of Section 508 of the US Rehabilitation Act, EN 301 549 and \
                      the Web Content Accessibility Guidelines, reported in the Voluntary \
                      Product Accessibility Template that the Information Technology \
                      Industry Council publishes. The evaluation covers the notebook \
                      workspace, the experiment views and the model registry. Each \
                      criterion is reported as supported, partially supported or not \
                      applicable, with the remarks the evaluator recorded for it. Customer \
                      teams use it to assess the product against their own accessibility \
                      obligations before they roll it out to their users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-Amazon-SageMaker-Studio.pdf",
        term_object: "Accessibility-Conformance-Report-Amazon-SageMaker-Studio-Term.txt",
        versions: &[
            SeedVersion {
                version: 1,
                period_start: T2024APR,
                period_end: T2025APR,
                published_at: T2024APR,
                sequence_number: 1,
            },
            SeedVersion {
                version: 2,
                period_start: T2025APR,
                period_end: T2026JAN,
                published_at: T2025APR,
                sequence_number: 2,
            },
        ],
    },
    SeedReport {
        id: "report-S6mFqH2xVb8JtZ4n",
        name: "Accessibility Conformance Report VPAT - Amazon Lex",
        description: "The Accessibility Conformance Report for Amazon Lex records how the \
                      product's user interfaces meet the accessibility criteria of Section \
                      508 of the US Rehabilitation Act, EN 301 549 and the Web Content \
                      Accessibility Guidelines, reported in the Voluntary Product \
                      Accessibility Template that the Information Technology Industry \
                      Council publishes. The evaluation covers the bot authoring surface, \
                      the test window and the conversation logs. Each criterion is reported \
                      as supported, partially supported or not applicable, with the remarks \
                      the evaluator recorded for it. Customer teams use it to assess the \
                      product against their own accessibility obligations before they roll \
                      it out to their users.",
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-Amazon-Lex.pdf",
        term_object: "Accessibility-Conformance-Report-Amazon-Lex-Term.txt",
        versions: &[SeedVersion {
            version: 1,
            period_start: T2025JAN,
            period_end: T2026JAN,
            published_at: T2025JAN,
            sequence_number: 1,
        }],
    },
];
