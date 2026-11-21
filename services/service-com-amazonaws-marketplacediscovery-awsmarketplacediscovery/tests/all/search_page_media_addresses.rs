// SPDX-License-Identifier: CC0-1.0

//! The addresses a SEARCH PAGE announces, page by page and refinement by refinement.
//!
//! The rule under test: the addresses a listing summary publishes for a client to FETCH are
//! read off that LISTING's own record and the endpoint the request reached this emulator at,
//! so they depend on neither the page a summary is served on, the page size it was asked for,
//! nor the query that selected it:
//!
//! * `logoThumbnailUrl` (`@required`) is the address this emulator serves that listing's logo
//!   media record at — one address per listing, naming no host this emulator is not reached
//!   on — and a fetch of it is answered here with the image;
//! * `ReviewSourceSummary$sourceUrl` (OPTIONAL) is announced only for a listing whose review
//!   source keeps a reviews page for it, is then THAT listing's own page on this emulator's
//!   endpoint — never one address shared by every summary — and a fetch of it is answered here
//!   with the page.
//!
//! [`crate::listing_media_addresses`] pins that rule on one page of the catalog and on the
//! round trip through `GetListing`/`GetProduct`. This suite pins the dimensions a paginating
//! and refining client walks instead: every page of a walk at every page size the model
//! allows, the continuation pages a token leads to, and the facet-refined and keyword-refined
//! queries a discovery client builds — the four shapes of `SearchListings` call whose summaries
//! announced a placeholder image host and one shared review landing page before
//! `issues/things-that-now-work/20261109T122558Z-marketplace-discovery-searchlistings-listing-summaries-announce-an-unservable-placeholder-logo.md`.
//!
//! Every request goes through the `MultiServiceRouter`, so the protocol, the routing table and
//! the handlers are exercised end to end, and each published address is fetched the way a
//! browser holding one does: anonymously, over the path alone.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::state::{
    logo_url, reviews_list_id, reviews_page_url, LogoMedia, REVIEW_SOURCE,
};

use crate::catalog_drilldown::{array, ok, text};
use crate::listing_media_addresses::{
    fetch, review_source, router_served_at, ENDPOINT_BASES, PNG_SIGNATURE, UNSERVED_HOSTS,
};
use crate::refined_search_pagination::facet_values;

/// `SearchListingsInput$maxResults` is `@range 1..100`
/// (`smithy_query(query="operation",operation="SearchListings")`): the largest page a client
/// can ask for, and the page the whole match set is read at below.
const MAX_PAGE: u64 = 100;

/// The page sizes the whole catalog is walked at: the smallest the model allows (so every page
/// but the first is a continuation page), two in between, the default 25 and the largest.
const PAGE_SIZES: [u64; 5] = [1, 2, 7, 25, MAX_PAGE];

/// The page sizes each refined query is walked at: the smallest and the default, which is
/// enough to carry a refined match set over a page boundary.
const REFINED_PAGE_SIZES: [u64; 2] = [1, 25];

/// How many pages a walk may take before it is treated as non-terminating. The catalog holds
/// far fewer listings than this, so a token that never retires fails here instead of looping.
const MAX_PAGES: usize = 1_000;

/// The two addresses a listing summary announces for a client to FETCH.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Announced {
    /// `logoThumbnailUrl`, which is required of every summary.
    logo: String,
    /// The `sourceUrl` of the summary's review source, absent for a listing the source keeps
    /// no reviews page for.
    reviews: Option<String>,
}

/// What `summary` announces for a client to fetch.
fn announced(summary: &Value) -> Announced {
    Announced {
        logo: text(summary, "logoThumbnailUrl"),
        reviews: review_source(summary).and_then(|(_source, url)| url),
    }
}

/// One page of `query`: its summaries and the continuation token it handed out.
async fn page(
    router: &MultiServiceRouter,
    query: &Value,
    max_results: u64,
    next_token: Option<&str>,
) -> (Vec<Value>, Option<String>) {
    let mut input = query.clone();
    input["maxResults"] = json!(max_results);
    if let Some(token) = next_token {
        input["nextToken"] = json!(token);
    }
    let body = ok(router, "searchListings", &input).await;
    let token = body
        .get("nextToken")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    (array(&body, "listingSummaries"), token)
}

/// Walk `query` page by page at `max_results`, returning every summary the walk was served, in
/// the order it saw them, each with the one-based number of the page it arrived on.
///
/// The rules about the WALK itself — that a token is handed out exactly while matches remain,
/// that it is a `NextToken` of the modelled shape, and that the pages carry the whole match set
/// once — belong to [`crate::refined_search_pagination`] and are not restated here: this suite
/// asserts what each page ANNOUNCES, so the loop only needs the summaries and a guard against
/// a token that never retires.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    query: &Value,
    max_results: u64,
) -> Vec<(usize, Value)> {
    let mut seen: Vec<(usize, Value)> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let (summaries, next) = page(router, query, max_results, token.as_deref()).await;
        pages += 1;
        assert!(
            pages <= MAX_PAGES,
            "case {case} maxResults={max_results}: the walk must terminate, got {pages} pages \
             after {} summaries",
            seen.len()
        );
        for summary in summaries {
            seen.push((pages, summary));
        }
        token = next;
        if token.is_none() {
            break;
        }
    }
    seen
}

/// The last `segments` path segments of `url`, lowercased: the part of a published address
/// that belongs to the RECORD (a media record's identifier and the image's file name, a reviews
/// list's identifier), the segments before it naming this service's own plane.
fn record_part(url: &str, segments: usize) -> String {
    let mut tail: Vec<&str> = url.rsplit('/').take(segments).collect();
    tail.reverse();
    tail.join("/").to_lowercase()
}

/// RULE: the record's own part of a published address carries an identifier this service issued
/// and none of the text the record is displayed by, so a client persisting the address holds a
/// handle rather than a form of a name (the defect published `…/logos/<name slug>.png`).
fn assert_record_part_names_no_listing_text(
    base: &str,
    listing_id: &str,
    listing_name: &str,
    url: &str,
    segments: usize,
) {
    let part = record_part(url, segments);
    for word in listing_name.to_lowercase().split(' ') {
        assert!(
            word.len() < 4 || !part.contains(word),
            "case {base:?} {listing_id}: the address's own part must carry nothing of the \
             listing's name, got {part:?} for {word:?} in {url:?}"
        );
    }
}

/// What every listing of the catalog announces, read at the largest page the model allows:
/// the addresses the rule says a summary carries, asserted here per listing so the comparisons
/// the suites below make are against values that already satisfy it.
async fn catalog_addresses(
    router: &MultiServiceRouter,
    base: &str,
) -> BTreeMap<String, Announced> {
    let (source_id, _source_name) = REVIEW_SOURCE;
    let mut catalog: BTreeMap<String, Announced> = BTreeMap::new();
    let mut logos: BTreeSet<String> = BTreeSet::new();
    let mut reviews: BTreeSet<String> = BTreeSet::new();
    let mut withheld = 0_usize;

    // The address of this emulator every announced URL has to begin with, whatever the record
    // part that follows it is.
    let endpoint = base.trim_end_matches('/');

    for (_page, summary) in walk(router, "whole catalog", &json!({}), MAX_PAGE).await {
        let listing_id = text(&summary, "listingId");
        let got = announced(&summary);
        // The logo is the address this emulator serves the media record the catalog holds for
        // THIS listing at, on the endpoint this request arrived on.
        let media_id = LogoMedia::of_listing(&listing_id).media_id().to_string();
        assert_eq!(
            got.logo,
            logo_url(base, &media_id),
            "case {base:?} {listing_id}: expected the logo announced at this emulator's own \
             endpoint, got {:?}",
            got.logo
        );
        // Asserted on the published string itself, so the two rules the defect broke hold
        // however the address is composed: it begins with the endpoint this request reached
        // this emulator at, and the part of the path that is the RECORD's carries none of the
        // text the listing is displayed by (the defect published `…/logos/<name slug>.png`).
        assert!(
            got.logo.starts_with(endpoint),
            "case {base:?} {listing_id}: an announced address must begin with the endpoint this \
             request reached this emulator at, got {:?}",
            got.logo
        );
        let name = text(&summary, "listingName");
        assert_record_part_names_no_listing_text(base, &listing_id, &name, &got.logo, 2);
        for host in UNSERVED_HOSTS {
            assert!(
                !got.logo.contains(host),
                "case {base:?} {listing_id}: a published address must name no host this \
                 emulator does not serve, got {:?}",
                got.logo
            );
        }
        assert!(
            logos.insert(got.logo.clone()),
            "case {base:?} {listing_id}: every listing announces a logo address of its own, got \
             {:?} twice",
            got.logo
        );

        match &got.reviews {
            Some(url) => {
                assert_eq!(
                    *url,
                    reviews_page_url(base, &reviews_list_id(&listing_id, source_id)),
                    "case {base:?} {listing_id}: expected this listing's own reviews page on \
                     this emulator's endpoint, got {url:?}"
                );
                assert!(
                    url.starts_with(endpoint),
                    "case {base:?} {listing_id}: an announced address must begin with the \
                     endpoint this request reached this emulator at, got {url:?}"
                );
                assert_record_part_names_no_listing_text(base, &listing_id, &name, url, 1);
                for host in UNSERVED_HOSTS {
                    assert!(
                        !url.contains(host),
                        "case {base:?} {listing_id}: a published address must name no host this \
                         emulator does not serve, got {url:?}"
                    );
                }
                assert!(
                    reviews.insert(url.clone()),
                    "case {base:?} {listing_id}: a reviews page is one listing's, got {url:?} \
                     announced for a second listing"
                );
            }
            None => withheld += 1,
        }
        assert!(
            catalog.insert(listing_id.clone(), got).is_none(),
            "case {base:?} {listing_id}: a listing is reported once by a walk of the catalog"
        );
    }

    assert!(
        !reviews.is_empty() && withheld > 0,
        "case {base:?}: the catalog must hold listings whose source keeps a reviews page and \
         listings whose source keeps none, so the presence of sourceUrl carries information, \
         got {} announced and {withheld} withheld",
        reviews.len()
    );
    catalog
}

/// RULE: a summary's published addresses are the LISTING's, so they are the same whichever page
/// of a walk the listing falls on and whatever page size the client asked for.
///
/// Rows: the whole catalog walked at every page size the model allows — 1 (every page after the
/// first is a continuation page), 2, 7, the default 25 and the largest 100 — each page's
/// summaries compared against what the listing announces when the whole match set is one page.
#[tokio::test]
async fn a_summarys_addresses_are_the_listings_whatever_page_it_is_served_on() {
    let base = ENDPOINT_BASES[0];
    let router = router_served_at(base);
    let catalog = catalog_addresses(&router, base).await;
    assert!(
        catalog.len() > PAGE_SIZES[0] as usize,
        "case setup: the catalog must hold more listings than one page, got {}",
        catalog.len()
    );

    for max_results in PAGE_SIZES {
        let walked = walk(&router, "whole catalog", &json!({}), max_results).await;
        let visited: BTreeSet<String> = walked
            .iter()
            .map(|(_page, summary)| text(summary, "listingId"))
            .collect();
        assert_eq!(
            visited.len(),
            walked.len(),
            "case maxResults={max_results}: no listing may be served twice by one walk, got {} \
             summaries over {} listings",
            walked.len(),
            visited.len()
        );
        assert_eq!(
            visited,
            catalog.keys().cloned().collect::<BTreeSet<String>>(),
            "case maxResults={max_results}: the pages must walk the whole catalog, got {} of {} \
             listings",
            visited.len(),
            catalog.len()
        );
        for (page_number, summary) in &walked {
            let listing_id = text(summary, "listingId");
            let got = announced(summary);
            let expected = &catalog[&listing_id];
            assert_eq!(
                &got, expected,
                "case maxResults={max_results} page {page_number} {listing_id}: the addresses a \
                 summary announces follow the listing rather than the page, expected \
                 {expected:?}, got {got:?}"
            );
        }
    }
}

/// RULE: refining a search narrows WHICH listings are reported and changes nothing about what
/// each reported listing announces.
///
/// Rows: a keyword search; a search refined by the first and the last value of every facet type
/// `SearchFacets` offers (`CATEGORY`, `PRICING_MODEL`, `PUBLISHER`,
/// `FULFILLMENT_OPTION_TYPE`); and a keyword search refined by a category, which is the
/// facet-refinement flow itself — each walked at the smallest page size and at the default.
#[tokio::test]
async fn a_refined_search_announces_the_addresses_of_the_listings_it_narrows_to() {
    let base = ENDPOINT_BASES[1];
    let router = router_served_at(base);
    let catalog = catalog_addresses(&router, base).await;

    let mut cases: Vec<(String, Value)> =
        vec![("searchText=data".to_string(), json!({ "searchText": "data" }))];
    // The refinements the service itself offers, read off `SearchFacets` rather than written
    // out here, so the table spans the facets a client can actually narrow by.
    let mut first_category = String::new();
    for facet_type in ["CATEGORY", "PRICING_MODEL", "PUBLISHER", "FULFILLMENT_OPTION_TYPE"] {
        let values = facet_values(&router, facet_type).await;
        let edges = [values.first().cloned(), values.iter().next_back().cloned()];
        if facet_type == "CATEGORY" {
            first_category = values
                .first()
                .cloned()
                .expect("the catalog offers a CATEGORY facet value")
                .0;
        }
        for (value, _count) in edges.into_iter().flatten() {
            let case = format!("{facet_type}={value}");
            if cases.iter().any(|(seen, _)| *seen == case) {
                continue;
            }
            cases.push((
                case,
                json!({
                    "filters": [{ "filterType": facet_type, "filterValues": [value] }]
                }),
            ));
        }
    }
    cases.push((
        format!("searchText=data refined by CATEGORY={first_category}"),
        json!({
            "searchText": "data",
            "filters": [{ "filterType": "CATEGORY", "filterValues": [first_category] }]
        }),
    ));

    let mut compared = 0_usize;
    for (case, query) in &cases {
        for max_results in REFINED_PAGE_SIZES {
            let walked = walk(&router, case, query, max_results).await;
            assert!(
                !walked.is_empty(),
                "case {case} maxResults={max_results}: a refinement the service offered must \
                 select listings, got none"
            );
            for (page_number, summary) in &walked {
                let listing_id = text(summary, "listingId");
                let got = announced(summary);
                let expected = catalog.get(&listing_id).unwrap_or_else(|| {
                    panic!(
                        "case {case} maxResults={max_results} page {page_number}: {listing_id} \
                         must be a listing of the catalog, got one the catalog walk never \
                         reported"
                    )
                });
                assert_eq!(
                    &got, expected,
                    "case {case} maxResults={max_results} page {page_number} {listing_id}: a \
                     refined search announces the listing's own addresses, expected {expected:?}, \
                     got {got:?}"
                );
                compared += 1;
            }
        }
    }
    assert!(
        compared >= cases.len(),
        "case table: every refinement must have contributed a summary to compare, got {compared} \
         over {} cases",
        cases.len()
    );
}

/// RULE: an address a page announces is served HERE — so a client walking page after page can
/// fetch the logo of every row it sees, and the reviews page of every row that announces one,
/// while a row that announces none has no page served at the address it would have had.
///
/// Rows: the whole catalog walked one listing per page, so every page but the first is reached
/// through a continuation token, over each endpoint base this emulator can be reached at.
#[tokio::test]
async fn every_page_of_a_token_walk_announces_addresses_this_emulator_serves() {
    let (source_id, _source_name) = REVIEW_SOURCE;
    for base in ENDPOINT_BASES {
        let router = router_served_at(base);
        let walked = walk(&router, "whole catalog", &json!({}), 1).await;
        let mut continuation_pages = 0_usize;
        let mut announced_pages = 0_usize;
        let mut withheld_pages = 0_usize;

        for (page_number, summary) in &walked {
            let listing_id = text(summary, "listingId");
            let got = announced(summary);
            assert_eq!(
                *page_number,
                continuation_pages + 1,
                "case {base:?} {listing_id}: a page of one carries one summary, got page \
                 {page_number} after {continuation_pages} pages"
            );
            continuation_pages += 1;

            let image = fetch(&router, "GET", &got.logo).await;
            assert_eq!(
                image.status, 200,
                "case {base:?} page {page_number} {listing_id}: expected the logo announced at \
                 {:?} to be served here, got {} ({:?})",
                got.logo,
                image.status,
                image.as_text()
            );
            assert_eq!(
                image.content_type, "image/png",
                "case {base:?} page {page_number} {listing_id}: expected the logo served as a \
                 PNG, got {:?}",
                image.content_type
            );
            assert!(
                image.body.starts_with(&PNG_SIGNATURE),
                "case {base:?} page {page_number} {listing_id}: expected PNG bytes at {:?}, got \
                 {} bytes starting {:?}",
                got.logo,
                image.body.len(),
                image.body.iter().take(8).collect::<Vec<_>>()
            );

            // The page this listing's reviews would be read at, whether or not its record
            // keeps one.
            let would_be = reviews_page_url(base, &reviews_list_id(&listing_id, source_id));
            let Some(published) = &got.reviews else {
                let answered = fetch(&router, "GET", &would_be).await;
                assert_ne!(
                    answered.status, 200,
                    "case {base:?} page {page_number} {listing_id}: a summary announcing no \
                     sourceUrl must have no reviews page served at {would_be:?}, got {} ({:?})",
                    answered.status,
                    answered.as_text()
                );
                withheld_pages += 1;
                continue;
            };
            assert_eq!(
                *published, would_be,
                "case {base:?} page {page_number} {listing_id}: expected this listing's own \
                 reviews page, got {published:?}"
            );
            let reviews = fetch(&router, "GET", published).await;
            assert_eq!(
                reviews.status, 200,
                "case {base:?} page {page_number} {listing_id}: expected the reviews page \
                 announced at {published:?} to be served here, got {} ({:?})",
                reviews.status,
                reviews.as_text()
            );
            assert!(
                reviews.content_type.starts_with("text/html"),
                "case {base:?} page {page_number} {listing_id}: expected a page a client can \
                 open, got content type {:?}",
                reviews.content_type
            );
            assert!(
                reviews.as_text().contains(listing_id.as_str()),
                "case {base:?} page {page_number} {listing_id}: expected the page to restate the \
                 listing it is the reviews of, got {:?}",
                reviews.as_text()
            );
            announced_pages += 1;
        }

        assert!(
            continuation_pages > 1 && announced_pages > 0 && withheld_pages > 0,
            "case {base:?}: the walk must cross page boundaries and cover summaries that \
             announce a reviews page and summaries that announce none, got \
             {continuation_pages} pages, {announced_pages} announcing and {withheld_pages} \
             withholding"
        );
    }
}
