// SPDX-License-Identifier: CC0-1.0

//! `ListDatabases` paging bounds, and the fact that the listing only REPORTS.
//!
//! The rule under test, from the operation's own documentation ("lists the databases in the
//! specified data catalog", and of `MaxResults`, "specifies the maximum number of results to
//! return") and from the model's `MaxResults` target `MaxDatabasesCount`, whose range is
//! 1..50: a page of this listing never holds more entries than that range's upper bound —
//! including when the request names no `MaxResults` at all, where the service pages at that
//! upper bound — a `MaxResults` outside the range is refused as invalid input while each
//! bound itself is served, and the returned `NextToken` resumes the catalog's own name order
//! strictly after the name the previous page ended on, whatever the catalog recorded in
//! between. The listing is a read: every database it reports is one the catalog itself
//! reports, and listing or paging leaves the catalog exactly as it was.
//!
//! Every database here is created over GLUE's real wire (`AWSGlue.CreateDatabase`) against
//! the same shared engine both services are mounted on, and every Athena call goes over
//! Athena's own wire through the same `MultiServiceRouter` the production front door uses —
//! so what is asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The suites are tables: each row is one input along the axis the rule generalises over
//! (the page size asked for, including none; the bound and one step past it; what the catalog
//! records between two pages), and the recorded case — a listing of a Glue catalog's
//! databases — is one row among them.

use serde_json::{json, Value};

use crate::harness::error_code;
use crate::metadata_database_listing::reported_names;
use crate::table_metadata::{athena, athena_ok, emulator, glue_ok, GLUE_CATALOG};

/// The database an account's Data Catalog HAS, which every listing reports beside the
/// databases a client created.
const CATALOG_DEFAULT_DATABASE: &str = "default";

/// The largest page this listing serves, and the size it pages at when the request names no
/// `MaxResults`. Source: the model's `MaxResults` target `MaxDatabasesCount`, whose range is
/// 1..50 (`smithy_query(query="shape",shape="MaxDatabasesCount")`).
const LARGEST_PAGE: usize = 50;

/// Create `count` databases in the account's Glue Data Catalog, named so that their stored
/// name order is known, and return every name the catalog then holds in that order.
async fn catalog_holding(
    router: &smithy_server_runtime::MultiServiceRouter,
    count: usize,
) -> Vec<String> {
    for index in 0..count {
        glue_ok(
            router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": format!("page_db_{index:03}") } }),
        )
        .await;
    }
    let mut names: Vec<String> = (0..count).map(|index| format!("page_db_{index:03}")).collect();
    names.push(CATALOG_DEFAULT_DATABASE.to_string());
    names.sort();
    names
}

/// Page the whole listing with the given request body, following every `NextToken`, and
/// return the names reported in order.
async fn page_everything(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    request: &Value,
    at_most: usize,
    total: usize,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut request = request.clone();
        if let Some(cursor) = &token {
            request["NextToken"] = json!(cursor);
        }
        let page = athena_ok(router, "ListDatabases", &request).await;
        let names = reported_names(&page);
        assert!(
            names.len() <= at_most,
            "case {case:?}: a page holds at most {at_most} databases, got {} in {names:?}",
            names.len()
        );
        seen.extend(names);
        pages += 1;
        assert!(
            pages <= total + 1,
            "case {case:?}: paging must terminate, saw {seen:?}"
        );
        match page["NextToken"].as_str() {
            Some(cursor) => token = Some(cursor.to_string()),
            None => break,
        }
    }
    seen
}

/// No page holds more databases than the model's page-size range admits — not even one the
/// request asked no size for — and following the token reports the catalog's whole order
/// exactly once.
///
/// The rows differ along the page size the request names: none at all, a size that divides
/// the stored count, one that does not, and the range's upper bound.
#[tokio::test]
async fn no_page_holds_more_databases_than_the_models_range_admits() {
    let router = emulator();
    // More databases than the largest page, so a single page can never report them all.
    let whole = catalog_holding(&router, LARGEST_PAGE + 7).await;
    assert_eq!(
        whole.len(),
        LARGEST_PAGE + 8,
        "setup: the catalog must hold the created databases and its own, got {whole:?}"
    );

    // (case, the MaxResults the request names, the most entries its first page may hold)
    // The row the model's own default decides — no `MaxResults` at all — is the axis this
    // suite adds; the smallest size the range admits is served by
    // `a_page_size_outside_the_models_range_is_refused` below.
    let rows: Vec<(&str, Option<i64>, usize)> = vec![
        ("no page size at all", None, LARGEST_PAGE),
        ("a page size that divides the stored count", Some(29), 29),
        ("a page size that does not divide it", Some(17), 17),
        ("the largest page size the range admits", Some(50), LARGEST_PAGE),
    ];

    for (case, max_results, at_most) in rows {
        let mut request = json!({ "CatalogName": GLUE_CATALOG });
        if let Some(size) = max_results {
            request["MaxResults"] = json!(size);
        }

        let first = athena_ok(&router, "ListDatabases", &request).await;
        let names = reported_names(&first);
        assert_eq!(
            names.len(),
            at_most,
            "case {case:?}: the first page of a catalog holding {} databases must hold \
             {at_most}, got {names:?}",
            whole.len()
        );
        assert!(
            first["NextToken"].as_str().is_some(),
            "case {case:?}: a listing that reported only {} of {} databases must name a next \
             page, got {first}",
            names.len(),
            whole.len()
        );

        let seen = page_everything(&router, case, &request, at_most, whole.len()).await;
        assert_eq!(
            seen, whole,
            "case {case:?}: paging must report the catalog's whole order exactly once each, \
             got {seen:?}"
        );
    }
}

/// A `MaxResults` outside the model's range is refused as invalid input, and each bound of
/// that range is served — the refusal starts one step past the bound, not at it.
///
/// The rows are the published bounds of `MaxDatabasesCount` (1..50) and one step past each.
#[tokio::test]
async fn a_page_size_outside_the_models_range_is_refused() {
    let router = emulator();
    let _ = catalog_holding(&router, 3).await;

    // (case, the MaxResults the request names, whether it is inside the range)
    let rows: Vec<(&str, i64, bool)> = vec![
        ("one below the range's lower bound", 0, false),
        ("the range's lower bound", 1, true),
        ("the range's upper bound", 50, true),
        ("one above the range's upper bound", 51, false),
    ];

    for (case, max_results, served) in rows {
        let request = json!({ "CatalogName": GLUE_CATALOG, "MaxResults": max_results });
        let (status, body) = athena(&router, "ListDatabases", &request).await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: a page size inside the range is served, got {status} with {body}"
            );
            assert!(
                !reported_names(&body).is_empty(),
                "case {case:?}: the served page reports the catalog's databases, got {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: a page size outside the range is refused, got {status} with \
                 {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidRequestException",
                "case {case:?}: expected InvalidRequestException, got {body}"
            );
        }
    }
}

/// One row of the resumption table: what the catalog records between two pages, and the
/// names the second page must then report.
struct Between {
    /// What the row is.
    case: &'static str,
    /// The databases created between the two pages.
    created: &'static [&'static str],
    /// The databases deleted between the two pages.
    deleted: &'static [&'static str],
    /// The names the resumed listing must then report, in order.
    expected: &'static [&'static str],
}

/// The token resumes the catalog's name order strictly after the name the previous page
/// ended on, whatever the catalog recorded between the two calls: a database stored before
/// that name is not reported again, one stored after it is reported, and one dropped after it
/// stops being reported.
///
/// The rows differ along what the catalog records between the two pages.
#[tokio::test]
async fn the_token_resumes_after_the_name_the_page_ended_on() {
    let rows: Vec<Between> = vec![
        Between {
            case: "nothing recorded between the two pages",
            created: &[],
            deleted: &[],
            expected: &["page_db_001", "page_db_002", "page_db_003"],
        },
        Between {
            case: "a database stored before the name the page ended on",
            created: &["aaa_before_cursor"],
            deleted: &[],
            expected: &["page_db_001", "page_db_002", "page_db_003"],
        },
        Between {
            case: "a database stored after the name the page ended on",
            created: &["page_db_004"],
            deleted: &[],
            expected: &["page_db_001", "page_db_002", "page_db_003", "page_db_004"],
        },
        Between {
            case: "a database dropped after the name the page ended on",
            created: &[],
            deleted: &["page_db_002"],
            expected: &["page_db_001", "page_db_003"],
        },
    ];

    for Between {
        case,
        created,
        deleted,
        expected,
    } in rows
    {
        let router = emulator();
        // The catalog's own database plus three, so a first page of two ends on
        // `page_db_000` and the rest follow it in name order.
        let _ = catalog_holding(&router, 4).await;

        let first = athena_ok(
            &router,
            "ListDatabases",
            &json!({ "CatalogName": GLUE_CATALOG, "MaxResults": 2 }),
        )
        .await;
        assert_eq!(
            reported_names(&first),
            [CATALOG_DEFAULT_DATABASE, "page_db_000"],
            "case {case:?}: the first page must report the first two stored names, got {first}"
        );
        let cursor = first["NextToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a truncated listing names a next page, got {first}"))
            .to_string();

        for name in created {
            glue_ok(
                &router,
                "CreateDatabase",
                &json!({ "DatabaseInput": { "Name": name } }),
            )
            .await;
        }
        for name in deleted {
            glue_ok(&router, "DeleteDatabase", &json!({ "Name": name })).await;
        }

        let rest = page_everything(
            &router,
            case,
            &json!({ "CatalogName": GLUE_CATALOG, "MaxResults": 2, "NextToken": cursor }),
            2,
            8,
        )
        .await;
        assert_eq!(
            rest, expected,
            "case {case:?}: the resumed listing must report the stored names after the \
             cursor, expected {expected:?}, got {rest:?}"
        );
    }
}

/// Every database the listing reports is one the CATALOG reports, and listing or paging
/// changes nothing a later caller sees: the same listing repeated answers the same page, and
/// the catalog's own read still reports the same databases.
#[tokio::test]
async fn the_listing_reports_the_catalogs_databases_and_changes_nothing() {
    let router = emulator();
    let whole = catalog_holding(&router, 6).await;

    // What the CATALOG itself reports, which is what the listing must report.
    let from_catalog = reported_names(&glue_ok(&router, "GetDatabases", &json!({})).await);
    assert_eq!(
        from_catalog, whole,
        "setup: the catalog must report the created databases and its own, got {from_catalog:?}"
    );

    // (case, the request whose answer must not depend on how often it is made)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a listing of every database",
            json!({ "CatalogName": GLUE_CATALOG }),
        ),
        (
            "a listing of one page",
            json!({ "CatalogName": GLUE_CATALOG, "MaxResults": 2 }),
        ),
        (
            // `WorkGroup` is "the name of the workgroup for which the metadata is being
            // fetched", so it selects no metadata of its own: the databases are the
            // catalog's.
            "a listing naming a workgroup",
            json!({ "CatalogName": GLUE_CATALOG, "WorkGroup": "primary" }),
        ),
    ];

    for (case, request) in &rows {
        let first = athena_ok(&router, "ListDatabases", request).await;
        // Page the whole listing, so every page of it is made at least once.
        let paged = page_everything(&router, case, request, whole.len(), whole.len()).await;
        assert_eq!(
            paged, whole,
            "case {case:?}: the listing must report the catalog's databases, got {paged:?}"
        );
        let again = athena_ok(&router, "ListDatabases", request).await;
        assert_eq!(
            reported_names(&again),
            reported_names(&first),
            "case {case:?}: repeating the listing must answer the same page, got {again}"
        );
    }

    let after = reported_names(&glue_ok(&router, "GetDatabases", &json!({})).await);
    assert_eq!(
        after, whole,
        "listing the databases must leave the catalog exactly as it was, got {after:?}"
    );
}
