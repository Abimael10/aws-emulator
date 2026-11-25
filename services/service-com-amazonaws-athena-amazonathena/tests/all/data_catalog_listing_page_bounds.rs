// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what BOUNDS a `ListDataCatalogs` page and for the listing being a
//! REPORTING read, driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: the listing reports the catalogs the account holds in one stable
//! order, a page of at most `MaxResults` entries at a time — and at most the modelled
//! maximum of that member where a request names none — handing back a continuation token
//! only where catalogs remain behind the page it reported, so that following the tokens
//! reports every catalog exactly once. Listing is a read: it reports the same page for the
//! same request and the same token however often it is called, and leaves every catalog
//! where a later caller still reads it.
//!
//! The bounds are the model's, read with
//! `smithy_query(query="operation",operation="ListDataCatalogs,CreateDataCatalog")`:
//! `MaxResults` is `MaxDataCatalogsCount` (range 2..50), so a page size at either published
//! bound is served and one step past either is outside the member's range — refused as
//! invalid input, like any other broken input constraint
//! (`tests/all/boundary_validation_refusal.rs`).
//!
//! `tests/all/data_catalog_listing.rs` asserts WHAT a page reports (the implicit Glue Data
//! Catalog, each registered catalog's type and status, a token no listing minted); this
//! suite asserts the bounds of a page over an account holding MORE catalogs than one page
//! can carry, which is where the page size a request omits and the walk across several
//! pages can be seen at all.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_CATALOG_TYPE_LAMBDA, ERROR_CODE_INVALID_INPUT, GLUE_DATA_CATALOG_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, message, router};

/// The largest page `MaxDataCatalogsCount` admits, which is also the page a request naming
/// no `MaxResults` gets.
const LARGEST_PAGE: usize = 50;

/// The smallest page `MaxDataCatalogsCount` admits.
const SMALLEST_PAGE: usize = 2;

/// How many catalogs the account under test registers: more than one largest page, so a
/// walk crosses pages and the page a request omits a size on is visibly bounded.
const REGISTERED: usize = LARGEST_PAGE + 5;

/// The `Name` of the `n`th catalog this suite registers.
///
/// Admitted by the create path's own name rule (alphanumerics plus `_`, `@` and `-`, up to
/// 127 characters), and zero-padded so that name order and registration order agree,
/// letting a walk be checked against a sorted expectation.
fn catalog_name(n: usize) -> String {
    format!("page_bounds_catalog_{n:03}")
}

/// Register [`REGISTERED`] catalogs of a type whose creation completes at once, so nothing
/// in this suite depends on a transition a clock owes.
async fn given_many_catalogs(router: &MultiServiceRouter) -> Vec<String> {
    for n in 0..REGISTERED {
        let name = catalog_name(n);
        let (status, created) = call(
            router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": {
                    "function": "arn:aws:lambda:us-east-1:111122223333:function:connector",
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the catalog {name} must be created, got {status} with {created:?}"
        );
    }
    // Every account also holds its own Glue Data Catalog, which no create registered.
    let mut held: Vec<String> = (0..REGISTERED).map(catalog_name).collect();
    held.push(GLUE_DATA_CATALOG_NAME.to_string());
    held.sort();
    held
}

/// One page of the listing: `(status, the names it reported, its NextToken)`.
async fn page(router: &MultiServiceRouter, request: &Value) -> (u16, Vec<String>, Option<String>) {
    let (status, body) = call(router, "ListDataCatalogs", request).await;
    let names = body["DataCatalogsSummary"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| {
            summary["CatalogName"]
                .as_str()
                .unwrap_or("<no CatalogName>")
                .to_string()
        })
        .collect();
    let token = body["NextToken"].as_str().map(str::to_string);
    (status, names, token)
}

/// Walk the whole listing at `request_page_size`, and answer with the names it reported in
/// order and how many pages it took.
async fn walk(router: &MultiServiceRouter, request_page_size: Option<usize>) -> (Vec<String>, usize) {
    let case = format!("page size {request_page_size:?}");
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let mut request = json!({});
        if let Some(size) = request_page_size {
            request["MaxResults"] = json!(size);
        }
        if let Some(token) = &token {
            request["NextToken"] = json!(token);
        }
        let (status, names, next) = page(router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must answer 200, got {status}"
        );
        let bound = request_page_size.unwrap_or(LARGEST_PAGE);
        assert!(
            names.len() <= bound,
            "case {case:?}: a page must carry at most {bound}, got {} ({names:?})",
            names.len()
        );
        pages += 1;
        walked.extend(names);
        token = next;
        // A caller following tokens must make progress, so the walk cannot take more pages
        // than there are catalogs to report.
        assert!(
            pages <= REGISTERED + 2,
            "case {case:?}: the walk must end, got {pages} pages reporting {walked:?}"
        );
        if token.is_none() {
            break;
        }
    }
    (walked, pages)
}

/// A request naming no page size is served the modelled maximum, and following the tokens
/// reports every catalog the account holds exactly once, in the same order whatever the
/// page size.
#[tokio::test]
async fn every_page_size_reports_every_catalog_exactly_once() {
    let router = router();
    let held = given_many_catalogs(&router).await;

    let (status, first, token) = page(&router, &json!({})).await;
    assert_eq!(
        status, 200,
        "the listing must answer 200, got {status} reporting {first:?}"
    );
    assert_eq!(
        first.len(),
        LARGEST_PAGE,
        "a request naming no page size must be served the modelled maximum, got {}",
        first.len()
    );
    assert!(
        token.is_some(),
        "an account holding {} catalogs has more behind the first page, got no token",
        held.len()
    );

    // (case, the page size the walk asks for) — the member's own bounds, a size between
    // them, a size that is exactly what the account holds, one above it, and none at all.
    let rows: Vec<(&str, Option<usize>)> = vec![
        ("no page size at all", None),
        ("the smallest page the member admits", Some(SMALLEST_PAGE)),
        ("a page size between the bounds", Some(7)),
        ("the largest page the member admits", Some(LARGEST_PAGE)),
    ];
    for (case, size) in rows {
        let (walked, pages) = walk(&router, size).await;
        assert_eq!(
            walked, held,
            "case {case:?}: the pages must concatenate to every catalog the account holds, \
             got {walked:?}"
        );
        let bound = size.unwrap_or(LARGEST_PAGE);
        let expected_pages = held.len().div_ceil(bound);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: {} catalogs in pages of {bound} must take {expected_pages} \
             pages, got {pages}",
            held.len()
        );
    }
}

/// A page size at a published bound of `MaxDataCatalogsCount` is served; one step past
/// either bound is outside the member's range and refused as invalid input.
#[tokio::test]
async fn a_page_size_past_a_published_bound_is_refused() {
    let router = router();
    given_many_catalogs(&router).await;

    // (case, MaxResults) — each published bound of the range 2..50.
    for (case, size) in [
        ("the smallest page the member admits", SMALLEST_PAGE),
        ("the largest page the member admits", LARGEST_PAGE),
    ] {
        let (status, names, _) = page(&router, &json!({ "MaxResults": size })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a page size at a published bound must be served, got {status}"
        );
        assert_eq!(
            names.len(),
            size,
            "case {case:?}: the page must carry what it asked for, got {names:?}"
        );
    }

    // (case, MaxResults, the clause the refusal states) — one step past each bound.
    let rows: Vec<(&str, i64, &str)> = vec![
        (
            "one below the smallest page",
            1,
            "Value at 'maxResults' failed to satisfy constraint: \
             Member must have value greater than or equal to 2",
        ),
        (
            "one above the largest page",
            51,
            "Value at 'maxResults' failed to satisfy constraint: \
             Member must have value less than or equal to 50",
        ),
    ];
    for (case, size, clause) in rows {
        let (status, body) = call(&router, "ListDataCatalogs", &json!({ "MaxResults": size })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {body:?}"
        );
        assert_eq!(
            body["AthenaErrorCode"].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected the input class, got {body:?}"
        );
        assert_eq!(
            message(&body),
            format!("1 validation error detected: {clause}"),
            "case {case:?}: expected the validation sentence, got {:?}",
            message(&body)
        );
    }
}

/// Listing reports, it does not consume: the same request and the same token report the
/// same page however often they are called, and every catalog is still there to read after
/// the whole listing has been walked.
#[tokio::test]
async fn listing_reports_the_same_page_and_consumes_nothing() {
    let router = router();
    let held = given_many_catalogs(&router).await;

    // (case, the request repeated) — the first page of each page size, and a page resumed
    // from a token this listing minted.
    let (_, _, token) = page(&router, &json!({ "MaxResults": SMALLEST_PAGE })).await;
    let resumed = token.unwrap_or_else(|| panic!("the first page of two must hand back a token"));
    let rows: Vec<(&str, Value)> = vec![
        ("a request naming no page size", json!({})),
        (
            "the smallest page the member admits",
            json!({ "MaxResults": SMALLEST_PAGE }),
        ),
        (
            "a page resumed from a token this listing minted",
            json!({ "MaxResults": SMALLEST_PAGE, "NextToken": resumed }),
        ),
    ];
    for (case, request) in rows {
        let (first_status, first_body) = call(&router, "ListDataCatalogs", &request).await;
        let (again_status, again_body) = call(&router, "ListDataCatalogs", &request).await;
        assert_eq!(
            first_status, 200,
            "case {case:?}: the listing must answer 200, got {first_status} with {first_body:?}"
        );
        assert_eq!(
            (again_status, &again_body),
            (first_status, &first_body),
            "case {case:?}: a repeated read must report the same page, got {again_body:?}"
        );
    }

    // Walking the whole listing leaves every registered catalog where a read still finds
    // it, and leaves the listing itself reporting the same catalogs.
    let (walked, _) = walk(&router, Some(SMALLEST_PAGE)).await;
    assert_eq!(
        walked, held,
        "the walk must report every catalog the account holds, got {walked:?}"
    );
    for name in [catalog_name(0), catalog_name(REGISTERED - 1)] {
        let (status, read) = call(&router, "GetDataCatalog", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {name:?}: the catalog must still be readable after the listing, got \
             {status} with {read:?}"
        );
        assert_eq!(
            read["DataCatalog"]["Name"].as_str(),
            Some(name.as_str()),
            "case {name:?}: the read must answer with that catalog, got {read:?}"
        );
    }
    let (walked_again, _) = walk(&router, Some(SMALLEST_PAGE)).await;
    assert_eq!(
        walked_again, held,
        "the listing must report the same catalogs after being walked, got {walked_again:?}"
    );
}
