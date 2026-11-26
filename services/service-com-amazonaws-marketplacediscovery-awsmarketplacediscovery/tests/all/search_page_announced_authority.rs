// SPDX-License-Identifier: CC0-1.0

//! The authority a SEARCH PAGE announces its fetchable addresses on, over the shapes of
//! `SearchListings` call a discovery client makes.
//!
//! The rule under test: an address a response carries for a client to DEREFERENCE —
//! `ListingSummary$logoThumbnailUrl` (`URL`, `@required`, `@length(1..2048)`,
//! `@pattern ^https?://.*$`) and `ReviewSourceSummary$sourceUrl` (the same `URL`, optional) —
//! is published on the scheme and authority the ANSWERING REQUEST arrived on where that
//! authority can lead a fetch back to this process, and on the authority this process is
//! SERVED at where it cannot; the record's own part of the address (the media id, the reviews
//! list id) is a property of the record and moves with neither.
//!
//! [`crate::announced_address_authority`] pins that rule over the authorities a client can
//! state ([`DIALLED`], the crate's one table of them) on ONE page shape (a single whole-catalog
//! page) and across the drill-down operations;
//! [`crate::search_page_media_addresses`] pins the record's own part over every page size and
//! refinement from ONE authority. This suite pins the CROSS of the two, which is what a search
//! page is answered under: each authority a client can state, times the four shapes of search
//! page a client is served — the default page, a facet-refined page, a keyword page, and the
//! first page of a paginated walk together with the continuation page its token leads to. A
//! continuation token carries no authority, so the page it leads to is announced on the
//! authority of ITS OWN request, which the second suite below pins by minting the token at one
//! authority and spending it at another.
//!
//! What a summary announces is asserted through
//! [`crate::announced_address_authority::assert_summary_addresses`], over the authority rows of
//! [`DIALLED`] — the one reading and the one table the two suites share, so neither can drift
//! from the other; this file contributes the shapes of REQUEST. Every request goes through the
//! `MultiServiceRouter`, so the protocol, the routing table and the handlers are exercised end
//! to end, and each published address is fetched the way a browser holding one does:
//! anonymously, over the path alone.

use std::collections::btree_map::{BTreeMap, Entry};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::announced_address_authority::{
    assert_summary_addresses, dialled_from, Dialled, DIALLED, SERVED_BASE,
};
use crate::catalog_drilldown::{array, text};
use crate::listing_media_addresses::{review_source, router_served_at};
use crate::refined_search_pagination::facet_values;

/// How many of a page's summaries have their announced addresses FETCHED. Every summary's
/// addresses are asserted; the fetches that prove the address is served at the authority it was
/// announced on are taken on the first few, which keeps the suite's call count bounded.
const FETCHED_PER_PAGE: usize = 2;

/// One `SearchListings` page, called as a client that reached this emulator the way `dialled`
/// states: the authority goes in the `Host` header, in the request target, or in both, exactly
/// as that row spells it.
async fn search_page(router: &MultiServiceRouter, dialled: Dialled, input: &Value) -> Value {
    dialled_from(
        router,
        dialled.host_header,
        dialled.target_authority,
        "searchListings",
        input,
    )
    .await
}

/// RULE, asserted over every summary of one answered page: each summary announces its own
/// listing's logo address, and the reviews page of a source that keeps one, on the authority
/// this request arrived on — and a fetch stating exactly that authority is answered here with
/// the image resp. the page.
///
/// Returns the page's summaries, so a row cannot pass on an empty page and the caller can
/// compare what one listing announces across rows.
async fn assert_page_announces_on(
    router: &MultiServiceRouter,
    case: &str,
    dialled: Dialled,
    page: &Value,
) -> Vec<Value> {
    let summaries = array(page, "listingSummaries");
    for (position, summary) in summaries.iter().enumerate() {
        let listing_id = text(summary, "listingId");
        assert_summary_addresses(
            router,
            &format!("{case} {listing_id}"),
            dialled.expected_base,
            dialled.stated(),
            summary,
            position < FETCHED_PER_PAGE,
        )
        .await;
    }
    summaries
}

/// The part of a published address AFTER the authority: the plane path and the record's own
/// identifiers. It belongs to the RECORD, so it is the same wherever the record is announced.
fn record_path(case: &str, base: &str, url: &str) -> String {
    url.strip_prefix(base)
        .unwrap_or_else(|| {
            panic!("case {case}: expected an address published on {base:?}, got {url:?}")
        })
        .to_string()
}

/// RULE: the part of the address after the authority identifies the RECORD — so one listing
/// announces the same path whichever authority its page was answered on, and no two listings
/// announce the same one.
///
/// Asserted on the published strings alone, independently of how the service derives them, so
/// a derivation that collapsed two records onto one address, or that moved a record's address
/// with the request, fails here.
fn assert_record_path(
    case: &str,
    member: &str,
    listing_id: &str,
    path: String,
    seen: &mut BTreeMap<String, String>,
) {
    match seen.entry(path) {
        Entry::Occupied(entry) => assert_eq!(
            entry.get(),
            listing_id,
            "case {case} {member}: {:?} is one record's address, announced for {listing_id} as \
             well as for {}",
            entry.key(),
            entry.get()
        ),
        Entry::Vacant(entry) => {
            entry.insert(listing_id.to_string());
        }
    }
}

/// The continuation token a page handed out, which is how the next page of the same query is
/// asked for.
fn continuation(page: &Value) -> Option<String> {
    page.get("nextToken")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// The queries whose pages are read below: the shapes of `SearchListings` call a discovery
/// client makes — the default page, a page refined by a facet the service itself offers, a
/// keyword page, and the first page of a walk whose page size is smaller than the match set
/// (whose continuation page is read through the token it hands out).
///
/// The facet value is read off `SearchFacets` rather than written here, so the refinement is
/// one the catalog actually offers.
async fn query_shapes(router: &MultiServiceRouter) -> Vec<(String, Value)> {
    let (category, _count) = facet_values(router, "CATEGORY")
        .await
        .first()
        .cloned()
        .expect("the catalog offers a CATEGORY facet value");
    vec![
        ("the default page".to_string(), json!({})),
        (
            format!("a page refined by CATEGORY={category}"),
            json!({ "filters": [{ "filterType": "CATEGORY", "filterValues": [category] }] }),
        ),
        (
            "a keyword search page".to_string(),
            json!({ "searchText": "data", "maxResults": 10 }),
        ),
        (
            "the first page of a paginated walk".to_string(),
            json!({ "maxResults": 1 }),
        ),
    ]
}

/// RULE: every shape of search page announces its fetchable addresses on the authority its own
/// request arrived on — the page a token leads to as much as the page a query starts with.
///
/// Rows: each authority a client can state ([`DIALLED`]) times each shape of query
/// ([`query_shapes`]), with the continuation page of every query that hands out a token read
/// from the same authority.
#[tokio::test]
async fn every_search_page_shape_announces_its_addresses_on_the_authority_it_arrived_on() {
    let router = router_served_at(SERVED_BASE);
    let shapes = query_shapes(&router).await;

    // Which listing each published PATH belongs to, accumulated over every row: the record's
    // own part of the address, read off the published strings rather than recomputed.
    let mut paths: BTreeMap<String, String> = BTreeMap::new();

    for dialled in DIALLED {
        for (shape, query) in &shapes {
            let case = format!("dialled {:?} / {shape}", dialled.case);
            let page = search_page(&router, dialled, query).await;
            let summaries = assert_page_announces_on(&router, &case, dialled, &page).await;
            assert!(
                !summaries.is_empty(),
                "case {case}: the seeded catalog must report listings for this query, got {page}"
            );
            assert_record_paths(&case, dialled.expected_base, &summaries, &mut paths);

            let Some(token) = continuation(&page) else {
                continue;
            };
            let mut next = query.clone();
            next["nextToken"] = json!(token);
            let page = search_page(&router, dialled, &next).await;
            let case = format!("{case} / its continuation page");
            let summaries = assert_page_announces_on(&router, &case, dialled, &page).await;
            assert!(
                !summaries.is_empty(),
                "case {case}: a continuation token must lead to a page of summaries, got {page}"
            );
            assert_record_paths(&case, dialled.expected_base, &summaries, &mut paths);
        }
    }
    // The table must have compared the addresses of more than one record, each of which has at
    // least one published address: a comparison over a single listing would say nothing about
    // whether an address identifies the record it belongs to.
    let listings: std::collections::BTreeSet<&String> = paths.values().collect();
    assert!(
        listings.len() > 1 && paths.len() >= listings.len(),
        "case table: the rows must have compared the published addresses of more than one \
         record, got {} addresses over {} listings",
        paths.len(),
        listings.len()
    );
}

/// [`assert_record_path`] over every summary of one page, for both members that carry an
/// address.
fn assert_record_paths(
    case: &str,
    base: &str,
    summaries: &[Value],
    paths: &mut BTreeMap<String, String>,
) {
    for summary in summaries {
        let listing_id = text(summary, "listingId");
        let logo = text(summary, "logoThumbnailUrl");
        assert_record_path(
            case,
            "logoThumbnailUrl",
            &listing_id,
            record_path(case, base, &logo),
            paths,
        );
        if let Some(url) = review_source(summary).and_then(|(_source, url)| url) {
            assert_record_path(
                case,
                "sourceUrl",
                &listing_id,
                record_path(case, base, &url),
                paths,
            );
        }
    }
}

/// RULE: a continuation token names a query, not an authority, so the page it leads to is
/// announced on the authority of the request that SPENT the token — not on the one that minted
/// it.
///
/// Rows: every ordered pair of authorities of [`DIALLED`] whose published bases differ, so each
/// pair mints a token at one authority and spends it at another.
#[tokio::test]
async fn a_continuation_page_is_announced_on_the_authority_that_spent_the_token() {
    let router = router_served_at(SERVED_BASE);
    let mut pairs = 0_usize;

    for minted_at in DIALLED {
        let first = search_page(&router, minted_at, &json!({ "maxResults": 1 })).await;
        let Some(token) = continuation(&first) else {
            panic!(
                "case {}: a page of one over the seeded catalog must hand out a continuation \
                 token, got {first}",
                minted_at.case
            );
        };
        for spent_at in DIALLED {
            if spent_at.expected_base == minted_at.expected_base {
                continue;
            }
            let page = search_page(
                &router,
                spent_at,
                &json!({ "maxResults": 1, "nextToken": token }),
            )
            .await;
            let case = format!(
                "token minted dialled {:?}, spent dialled {:?}",
                minted_at.case, spent_at.case
            );
            let summaries = assert_page_announces_on(&router, &case, spent_at, &page).await;
            assert!(
                !summaries.is_empty(),
                "case {case}: the continuation page must carry the next summary, got {page}"
            );
            pairs += 1;
        }
    }
    assert!(
        pairs > 0,
        "case table: the rows must cover a token minted at one authority and spent at another, \
         got {pairs} pairs"
    );
}
