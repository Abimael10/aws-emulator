// SPDX-License-Identifier: CC0-1.0

//! The accessibility-conformance class of the published report catalog: the Accessibility
//! Conformance Report (the Voluntary Product Accessibility Template) AWS publishes for a
//! product it has evaluated.
//!
//! This class lives in its own file because of how it is SHAPED. The rest of the catalog
//! ([`super::SEED`]) is one row per compliance programme — one certificate, one
//! attestation, one questionnaire — while an accessibility-conformance report is published
//! PER PRODUCT, so the class is a set of rows that grows with the products AWS evaluates
//! and shares one series, one category, one acceptance type and one published description
//! between all of them. Those are therefore stated once here rather than repeated on every
//! row.
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
//! What each member CARRIES is the published metadata of the document the row's id names,
//! never prose re-authored about the product: every row is TITLED the way AWS titles this
//! class — the kind of document, the template it is reported on, then the product evaluated
//! ("Accessibility Conformance Report (VPAT) - <product>", as the recorded catalog entry of
//! this class is titled) — and carries the published description of the template
//! ([`ACCESSIBILITY_DESCRIPTION`]) and the cycle the class is published on (the window
//! constants each version names). A published title carries punctuation that
//! `ShortStringAttribute`'s `@pattern` — a constraint on what a REQUEST may send — does not
//! admit; a response reports the title as it is published, which is what the recorded
//! catalog entry of this class shows. Each `description` satisfies `LongStringAttribute`'s
//! `^[^<>]*$` within 1..=2048 characters.

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

/// The published description every report of this class carries.
///
/// ONE text for the whole class rather than one per product: the publisher's description is
/// written about the template, not about a product — it states what an ACR is, which
/// standards the VPAT references, and that it informs about "the relevant AWS product or
/// service" — so which product a row reports on is carried by its title, never by this
/// text. This is the description the corpus records for an entry of this class.
const ACCESSIBILITY_DESCRIPTION: &str =
    "This document is an Accessibility Conformance Report (ACR) based on the ITI VPAT® \
     and is intended to provide information on the accessibility of the relevant AWS \
     product or service. The VPAT references accessibility standards for information and \
     communications technology including: Section 508 (U.S.), EN 301 549 (EU), and \
     W3C/WAI WCAG.";

/// The accessibility-conformance reports the catalog publishes, one per evaluated product.
///
/// The rows differ along what the report surface generalises over: the product evaluated,
/// the number of published versions (one, two and three) and the windows those versions
/// cover. A window is the one the evaluation it reports covers — a property of that
/// document rather than of the class — and the only one the corpus discloses is the AWS
/// Transfer Family report's, which opens on 1 November.
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
        name: "Accessibility Conformance Report (VPAT) - Amazon Connect",
        description: ACCESSIBILITY_DESCRIPTION,
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
        name: "Accessibility Conformance Report (VPAT) - AWS Transfer Family",
        description: ACCESSIBILITY_DESCRIPTION,
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-AWS-Transfer-Family.pdf",
        term_object: "Accessibility-Conformance-Report-AWS-Transfer-Family-Term.txt",
        // The window the corpus records for this report opens on 1 November 2024; its
        // close is the one this catalog publishes, which the recording does not contradict.
        versions: &[SeedVersion {
            version: 1,
            period_start: T2024NOV,
            period_end: T2025JAN,
            published_at: T2024NOV,
            sequence_number: 1,
        }],
    },
    SeedReport {
        id: "report-M2hXsL5bQw9TnF4j",
        name: "Accessibility Conformance Report (VPAT) - Amazon WorkSpaces",
        description: ACCESSIBILITY_DESCRIPTION,
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
        name: "Accessibility Conformance Report (VPAT) - Amazon QuickSight",
        description: ACCESSIBILITY_DESCRIPTION,
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
        name: "Accessibility Conformance Report (VPAT) - Amazon Chime SDK",
        description: ACCESSIBILITY_DESCRIPTION,
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
        name: "Accessibility Conformance Report (VPAT) - AWS CloudShell",
        description: ACCESSIBILITY_DESCRIPTION,
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
        name: "Accessibility Conformance Report (VPAT) - Amazon SageMaker Studio",
        description: ACCESSIBILITY_DESCRIPTION,
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
        name: "Accessibility Conformance Report (VPAT) - Amazon Lex",
        description: ACCESSIBILITY_DESCRIPTION,
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
