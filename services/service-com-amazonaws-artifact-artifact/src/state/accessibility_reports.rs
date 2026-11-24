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
//! part of it. Three rows below carry the ids the corpus discloses for an
//! accessibility-conformance report — `report-7g5j2DTVeNXWx7St` at one published version,
//! `report-0QjvdIs6Bsw4uPxb` at two and `report-0c1IXmudeXuTlHJJ` at three — which are
//! AWS's own, as a recorded `ListReports` body shows them; the remaining rows carry ids
//! this catalog assigns (they satisfy `ReportId`'s `^report-[a-zA-Z0-9]{16}$` and are not a
//! claim that AWS issued them), as [`super::SEED`]'s own doc comment explains for the rows
//! there.
//!
//! An id the corpus discloses names ONE published report, so the row that carries it is
//! the report the recording reports under it: a listing projects each entry from the row
//! its id names, which is why `report-0c1IXmudeXuTlHJJ` is seeded here, in the class the
//! recording reports it in, rather than beside the compliance programmes.
//!
//! What each member CARRIES is the published metadata of the document the row's id names,
//! never prose re-authored about the product: every row is TITLED the way AWS titles this
//! class — the kind of document, the template it is reported on, then the product evaluated
//! ("Accessibility Conformance Report (VPAT) - <product>", as the recorded catalog entry of
//! this class is titled) — and carries the published description of the template
//! ([`ACCESSIBILITY_DESCRIPTION`]) and the cycle the class is published on (the batch each
//! version names). A published title carries punctuation that
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

/// Where the material of this class is PUBLISHED: AWS publishes the accessibility
/// conformance of its products under one page, so every row of the class carries that one
/// link, the same way they share one description. It is what a citation of such a report
/// reports as its `sourceLink` ([`super::Report::source_link`]).
const ACCESSIBILITY_SOURCE_LINK: &str = "https://aws.amazon.com/accessibility/";

/// One publication cycle of this class: when a batch of accessibility-conformance reports
/// is published, and the instant the versions it publishes are effective to.
///
/// The class is published in BATCHES — AWS evaluates its products and publishes the
/// conformance reports of a round together — so a version's window opens on the batch it
/// was published in and closes on the 1 January that follows it, which is the pair the
/// recorded catalog entries of this class carry: one of them opens on 1 November 2024 and
/// closes on 1 January 2025, and another opens on 1 October 2025 and closes on 1 January
/// 2026. A row publishing several versions carries the last that many batches, so the
/// version a catalog listing reports is the one of the newest batch that evaluated it.
struct Batch {
    /// When the batch was published, which opens the window its versions cover.
    published_at: u64,
    /// The 1 January the batch's versions are effective to.
    effective_to: u64,
}

/// The batch published in the fourth quarter of 2022.
const BATCH_2022: Batch = Batch {
    published_at: T2022NOV,
    effective_to: T2023JAN,
};
/// The batch published in the fourth quarter of 2023.
const BATCH_2023: Batch = Batch {
    published_at: T2023NOV,
    effective_to: T2024JAN,
};
/// The batch published in the fourth quarter of 2024 — the one the recorded entry of the
/// AWS Transfer Family report is published in.
const BATCH_2024: Batch = Batch {
    published_at: T2024NOV,
    effective_to: T2025JAN,
};
/// The batch published in the fourth quarter of 2025 — the one the recorded entry of the
/// Amazon Polly report is published in.
const BATCH_2025: Batch = Batch {
    published_at: T2025OCT,
    effective_to: T2026JAN,
};

/// One version of a report of this class: `version`, published in `batch`.
const fn published_in(version: i64, batch: Batch) -> SeedVersion {
    SeedVersion {
        version,
        period_start: batch.published_at,
        period_end: batch.effective_to,
        published_at: batch.published_at,
        // A version is the n-th document published for the report, so the sequence number
        // the optimistic-locking member reports is that version's own position.
        sequence_number: version,
    }
}

/// The accessibility-conformance reports the catalog publishes, one per evaluated product.
///
/// The rows differ along what the report surface generalises over: the product evaluated,
/// the number of published versions (one, two and three) and the publication batches those
/// versions were published in. Which batches a row carries is a property of that document —
/// AWS evaluates a product in the rounds it evaluates it in — and the ones the corpus
/// discloses are the AWS Transfer Family report's single version, published in the fourth
/// quarter of 2024, and the Amazon Polly report's third, published in the fourth quarter of
/// 2025.
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[published_in(1, BATCH_2023), published_in(2, BATCH_2024)],
    },
    SeedReport {
        // The id the recorded `ListReports` body names this report under: the recording
        // reports it as the accessibility-conformance report of Amazon Polly at its third
        // published version, so that id names THIS row and the members a listing projects
        // under it are this row's.
        id: "report-0c1IXmudeXuTlHJJ",
        name: "Accessibility Conformance Report (VPAT) - Amazon Polly",
        description: ACCESSIBILITY_DESCRIPTION,
        series: ACCESSIBILITY_SERIES,
        category: ACCESSIBILITY_CATEGORY,
        acceptance_type: ACCESSIBILITY_ACCEPTANCE,
        document_object: "Accessibility-Conformance-Report-Amazon-Polly.pdf",
        term_object: "Accessibility-Conformance-Report-Amazon-Polly-Term.txt",
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[
            published_in(1, BATCH_2023),
            published_in(2, BATCH_2024),
            published_in(3, BATCH_2025),
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[published_in(1, BATCH_2024)],
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[published_in(1, BATCH_2023), published_in(2, BATCH_2024)],
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[published_in(1, BATCH_2024)],
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[
            published_in(1, BATCH_2022),
            published_in(2, BATCH_2023),
            published_in(3, BATCH_2024),
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[published_in(1, BATCH_2025)],
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[published_in(1, BATCH_2024), published_in(2, BATCH_2025)],
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
        source_link: ACCESSIBILITY_SOURCE_LINK,
        versions: &[published_in(1, BATCH_2025)],
    },
];
