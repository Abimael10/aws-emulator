// SPDX-License-Identifier: CC0-1.0

//! What the CATALOG `ListReports` enumerates HOLDS, end to end over HTTP.
//!
//! RULE — `ListReports` is the enumeration of the service's whole published report
//! inventory: every report the service holds appears on exactly ONE page of the
//! enumeration, carrying its full `ReportSummary`, whatever `maxResults` cuts the pages
//! at. So the union of the pages of a walk is the inventory itself — not the first page's
//! worth of it — and a report that the single-report reads resolve is never one a client
//! paging to the end fails to see.
//!
//! AWS Artifact has no `CreateReport`, so membership is not something a caller brings
//! about and not something the model bounds: what the catalog holds is the published set.
//! That set has two CLASSES, which is what this suite checks the enumeration covers — one
//! report per compliance programme, and one report per product AWS has evaluated for
//! accessibility conformance, a class AWS publishes a report of for each of its products.
//! A catalog that holds one row per programme and nothing per product enumerates a short
//! inventory, and a client that pages it to the end caches a catalog missing every
//! accessibility-conformance document with no error to retry on.
//!
//! The rows span what the rule generalises over: the page size the walk is cut at (both
//! published bounds of `MaxResultsAttribute`'s `@range(min: 1, max: 300)` and a size
//! between them that cuts the catalog mid-page), the report class, the series, and the
//! number of versions a report publishes — the reported entry is the LATEST one, so a
//! report's version count is part of what the enumeration has to get right.
//!
//! What a cursor MEANS, and the refusal of a value no page cut, is the pagination rule and
//! lives in `pagination_page_walk.rs`; a page size outside the modelled range is the front
//! door's refusal and lives in `front_door.rs` and `report_workflow.rs`. This suite is
//! about what the pages CONTAIN.

use std::collections::BTreeSet;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    emulator, get_report_metadata, list_report_versions, page_size_case, walk_reports, Expected,
    CATALOG, HOME_REGION,
};

/// The page sizes the enumeration is walked at: the floor and the ceiling of
/// `MaxResultsAttribute`'s `@range(min: 1, max: 300)`
/// (`smithy_query(query="shape",shape="MaxResultsAttribute")`), a size between them that
/// cuts the catalog mid-page, and the unpaged call (no `maxResults` at all).
const PAGE_SIZES: &[Option<i32>] = &[None, Some(1), Some(5), Some(300)];

/// The series the catalog publishes a report under, and whether the series is the
/// accessibility-conformance class (one report per evaluated product) rather than a
/// compliance programme (one report for the programme).
///
/// Source: the `series` member of the catalog rows this emulator publishes
/// (`harness::CATALOG`, which mirrors `SEED` and `accessibility_reports` in
/// `src/state.rs`); `ACR` is the series the recorded Artifact catalog names its
/// accessibility-conformance entries in. Spelled out here so a catalog reseeded without a
/// whole series fails this suite rather than redefining what it asserts.
const SERIES: &[(&str, Class)] = &[
    ("AWS Services", Class::Programme),
    ("ACR", Class::PerProduct),
    ("ISO", Class::Programme),
    ("UAE TDRA IAR", Class::Programme),
    ("SOC", Class::Programme),
    ("PCI", Class::Programme),
    ("FedRAMP", Class::Programme),
    ("IRAP", Class::Programme),
    ("C5", Class::Programme),
    ("CSA", Class::Programme),
];

/// Which class of the published inventory a series belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// One report for a compliance programme.
    Programme,
    /// One report per product AWS evaluated — so the series holds several reports.
    PerProduct,
}

/// `PublishedState$PUBLISHED`, the state of every report in the published catalog.
const PUBLISHED: &str = "PUBLISHED";

/// `AcceptanceType$PASSTHROUGH`: the acceptance type the recorded catalog entries of the
/// accessibility-conformance class carry — the class is published for everyone, so no
/// click-through acceptance of its term is required of a downloader.
const PASSTHROUGH: &str = "PASSTHROUGH";

/// The category the accessibility-conformance class is published in, as the recorded
/// entries of that class carry it.
const CERTIFICATIONS: &str = "Certifications and Attestations";

/// The members a catalog entry has to carry for the entry to BE a report summary rather
/// than a stub naming one. `uploadState` and `statusMessage` are not among them: the
/// catalog projection reports neither (see `report_projection.rs`), the single-report
/// metadata read does.
const SUMMARY_MEMBERS: &[&str] = &[
    "id",
    "arn",
    "name",
    "series",
    "category",
    "companyName",
    "productName",
    "description",
    "state",
    "version",
    "periodStart",
    "periodEnd",
    "acceptanceType",
];

/// One entry of the enumeration, as the wire reported it.
#[derive(Debug, Clone)]
struct Entry {
    id: String,
    version: i64,
    arn: String,
    series: String,
    category: String,
    acceptance_type: String,
    state: String,
    /// Which page of the walk reported it, so a report enumerated twice names both pages.
    page: usize,
}

/// Every entry of one whole walk of the enumeration, a page at a time.
///
/// The walk itself is the harness's ([`walk_reports`]), which asserts what makes it a walk
/// — each page served and cut at `maxResults`, and the walk terminating; this reads each
/// reported summary into an [`Entry`], so the membership rules below assert over the
/// entries rather than over the paging.
async fn walk(router: &MultiServiceRouter, case: &str, max_results: Option<i32>) -> Vec<Entry> {
    walk_reports(router, case, max_results)
        .await
        .into_iter()
        .map(|(page, summary)| entry(case, &summary, page))
        .collect()
}

/// One reported summary read into an [`Entry`], asserting it carries every member a report
/// summary carries — an entry missing one is an entry that names a report without
/// reporting it.
fn entry(case: &str, summary: &Value, page: usize) -> Entry {
    for member in SUMMARY_MEMBERS {
        let value = &summary[*member];
        assert!(
            !value.is_null() && value.as_str() != Some(""),
            "case {case:?}: an enumerated report carries {member:?}, got {summary:?}"
        );
    }
    Entry {
        id: summary["id"].as_str().unwrap_or_default().to_string(),
        version: summary["version"].as_i64().unwrap_or_default(),
        arn: summary["arn"].as_str().unwrap_or_default().to_string(),
        series: summary["series"].as_str().unwrap_or_default().to_string(),
        category: summary["category"].as_str().unwrap_or_default().to_string(),
        acceptance_type: summary["acceptanceType"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        state: summary["state"].as_str().unwrap_or_default().to_string(),
        page,
    }
}

/// The catalog row `id` names, or a failure saying the enumeration reported a report the
/// published set does not hold.
fn row(case: &str, id: &str) -> &'static Expected {
    CATALOG.iter().find(|row| row.id == id).unwrap_or_else(|| {
        panic!("case {case:?}: the enumeration reported {id:?}, which the catalog does not hold")
    })
}

#[tokio::test]
async fn every_catalog_report_is_enumerated_on_exactly_one_page_at_every_page_size() {
    let router = emulator();

    for max_results in PAGE_SIZES {
        let case = page_size_case(*max_results);
        let entries = walk(&router, &case, *max_results).await;

        // Exactly once: a report the walk reported twice is one a client pages into its
        // catalog twice, and a report it reported never is one the client never sees.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for found in &entries {
            assert!(
                seen.insert(found.id.as_str()),
                "case {case:?}: {} is enumerated more than once, again on page {}",
                found.id,
                found.page
            );
        }
        let enumerated: Vec<&str> = seen.iter().copied().collect();
        let held: Vec<&str> = CATALOG.iter().map(|row| row.id).collect();
        assert_eq!(
            enumerated, held,
            "case {case:?}: the union of the pages must be the whole published catalog, got \
             {enumerated:?}"
        );

        // And each entry is the report's LATEST published version, keyed by the ARN that
        // names that version.
        for found in &entries {
            let expected = row(&case, &found.id);
            assert_eq!(
                found.version,
                expected.latest(),
                "case {case:?}: {} is enumerated at its latest published version, got {:?}",
                found.id,
                found
            );
            assert_eq!(
                found.arn,
                format!(
                    "arn:aws:artifact:{HOME_REGION}::report/{}:{}",
                    found.id, found.version
                ),
                "case {case:?}: an enumerated report is keyed by its own ARN, got {:?}",
                found
            );
            assert_eq!(
                found.state, PUBLISHED,
                "case {case:?}: a report in the published catalog is {PUBLISHED}, got {:?}",
                found
            );
        }
    }
}

#[tokio::test]
async fn the_enumeration_covers_every_published_series_and_the_whole_per_product_class() {
    let router = emulator();
    let entries = walk(&router, "series coverage", None).await;

    for (series, class) in SERIES {
        let of_series: Vec<&Entry> = entries
            .iter()
            .filter(|found| found.series == *series)
            .collect();
        assert!(
            !of_series.is_empty(),
            "case {series:?}: the enumeration must cover every series the catalog publishes, \
             got {:?}",
            entries.iter().map(|found| &found.series).collect::<Vec<_>>()
        );
        if *class == Class::PerProduct {
            // AWS publishes one report of this class per product it evaluated, so the
            // series holds several reports rather than one standing for the class.
            assert!(
                of_series.len() > 1,
                "case {series:?}: a per-product class is a SET of reports, got {of_series:?}"
            );
            // Every row of the class, not only the one a recording happened to name,
            // carries what the recorded entries of the class carry.
            for found in &of_series {
                assert_eq!(
                    found.acceptance_type, PASSTHROUGH,
                    "case {series:?}: {} is published for everyone, so its acceptanceType is \
                     {PASSTHROUGH}, got {found:?}",
                    found.id
                );
                assert_eq!(
                    found.category, CERTIFICATIONS,
                    "case {series:?}: {} is published in {CERTIFICATIONS:?}, got {found:?}",
                    found.id
                );
            }
        }
    }
}

#[tokio::test]
async fn every_enumerated_report_resolves_through_the_single_report_reads() {
    let router = emulator();

    // The enumeration is the discovery path: a client reads the catalog, then names what
    // it found to the per-report reads. So every entry the walk reports has to be backed
    // by the record those reads serve, at the version the entry names.
    for found in walk(&router, "round trip", Some(4)).await {
        let case = format!("{} v{}", found.id, found.version);
        let metadata = get_report_metadata(
            &router,
            &format!("?reportId={}&reportVersion={}", found.id, found.version),
        )
        .await;
        assert_eq!(
            metadata.status, 200,
            "case {case:?}: an enumerated report must resolve, got {} with {:?}",
            metadata.status, metadata.body
        );
        let detail = &metadata.body["reportDetails"];
        assert_eq!(
            detail["arn"].as_str().unwrap_or_default(),
            found.arn,
            "case {case:?}: the per-report read must name the ARN the enumeration reported, \
             got {detail:?}"
        );
        assert_eq!(
            detail["series"].as_str().unwrap_or_default(),
            found.series,
            "case {case:?}: the two reads report one record, got {detail:?}"
        );

        // And the report's own version listing holds the enumerated version, which is why
        // the catalog entry reports that one.
        let versions = list_report_versions(&router, &format!("?reportId={}", found.id)).await;
        assert_eq!(
            versions.status, 200,
            "case {case:?}: the version listing must be served, got {} with {:?}",
            versions.status, versions.body
        );
        let published: Vec<i64> = versions
            .reports()
            .iter()
            .map(|summary| summary["version"].as_i64().unwrap_or_default())
            .collect();
        assert_eq!(
            published,
            row(&case, &found.id).versions.to_vec(),
            "case {case:?}: every published version of an enumerated report must be listed, \
             got {published:?}"
        );
        assert_eq!(
            published.iter().copied().next_back(),
            Some(found.version),
            "case {case:?}: the enumeration reports the newest published version, got \
             {published:?}"
        );
    }
}
