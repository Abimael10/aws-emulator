// SPDX-License-Identifier: CC0-1.0

//! The AUTHORITY every address this service publishes for a client to FETCH is announced on.
//!
//! The rule under test: an address a response carries for a client to dereference —
//! `ListingSummary$logoThumbnailUrl` and `GetListing`/`GetProduct`'s own
//! (`URL`, `@required`, `@length(1..2048)`, `@pattern ^https?://.*$`),
//! `ReviewSourceSummary$sourceUrl` (the same `URL`, optional) and a SaaS fulfillment option's
//! `fulfillmentUrl` — is published on the SOCKET this process is served on, because that is
//! the socket this emulator answers a fetch of it at:
//!
//! * a client that dialled that socket is handed its own spelling of it, so the name it
//!   resolved to get here is the name it dials again;
//! * a request that states any OTHER authority — a port this process does not serve (a
//!   forwarding front end in between, an address left over from an earlier run), an
//!   authority naming no port, or one of the hosts AWS publishes its own endpoints at — is
//!   answered with the address this process is served at, never with the authority it
//!   carried, since nothing answers a fetch there.
//!
//! [`crate::listing_media_addresses`] and [`crate::search_page_media_addresses`] pin which
//! RECORD each published address belongs to and that a fetch of it is served here; this suite
//! pins the one thing that is a property of the PROCESS rather than of the record — which
//! authority the address is published on — over the authorities a client can state and over
//! every operation of this service that publishes one.
//!
//! Every request goes through the `MultiServiceRouter`, so the protocol, the routing table and
//! the handlers are exercised end to end, and each published address is fetched the way a
//! browser holding one does: anonymously, over the path alone.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::state::{
    logo_url, reviews_list_id, reviews_page_url, LogoMedia, REVIEW_SOURCE,
};

use crate::catalog_drilldown::{array, text, AUTHORIZATION};
use crate::listing_media_addresses::{fetch, review_source, router_served_at, PNG_SIGNATURE};

/// The base this emulator is SERVED at in every row below: the one socket it accepts requests
/// on, which is the address a published URL has to name.
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// The authorities a client can state, each with the base the addresses of the response it
/// gets have to be published on.
///
/// The rows differ along what the rule generalises over: the served authority itself, another
/// spelling of that same socket, a port this process does not serve (the authority of a
/// forwarding front end, which is the shape of the defect), an authority naming no port at
/// all, a loopback name on another port, a host AWS publishes its own endpoints at, and one of
/// those hosts carrying the served port.
const DIALLED: [(&str, &str); 7] = [
    ("127.0.0.1:9297", "https://127.0.0.1:9297"),
    ("localhost:9297", "https://localhost:9297"),
    ("127.0.0.1:38377", SERVED_BASE),
    ("127.0.0.1", SERVED_BASE),
    ("localhost:39355", SERVED_BASE),
    ("discovery-marketplace.us-east-1.amazonaws.com", SERVED_BASE),
    (
        "discovery-marketplace.us-east-1.amazonaws.com:9297",
        SERVED_BASE,
    ),
];

/// How many of a page's summaries have their published addresses FETCHED per row. Every
/// summary's addresses are asserted; the fetches that prove the address is served here are
/// taken on the first few, which keeps the suite's call count bounded.
const FETCHED_PER_ROW: usize = 3;

/// The largest page `SearchListingsInput$maxResults` admits (`@range 1..100`), so one call
/// reports as much of the catalog as a page can carry.
const MAX_PAGE: u64 = 100;

/// Call `operation` as a client that DIALLED `host`: the authority it resolved to get here,
/// carried both in the `Host` header and in the request URI, exactly as a client reaching the
/// emulator over a socket states it.
async fn dialled_at(
    router: &MultiServiceRouter,
    host: &str,
    operation: &str,
    input: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/2026-02-05/{operation}"))
        .header("host", host)
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(input.to_string()))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "case dialled {host} {operation}: expected HTTP 200, got {status} ({:?})",
        String::from_utf8_lossy(response.body())
    );
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

/// RULE: every published address of a response satisfies the `URL` shape's own constraints —
/// `@length(1..2048)` and `@pattern ^https?://.*$` — whatever authority it is published on.
fn assert_is_a_url(case: &str, member: &str, url: &str) {
    assert!(
        (url.starts_with("https://") || url.starts_with("http://"))
            && (1..=2048).contains(&url.len()),
        "case {case} {member}: a published address must be a URL of 1..2048 characters matching \
         ^https?://.*$, got {url:?} ({} characters)",
        url.len()
    );
}

/// RULE: a published address is announced on `expected_base` and on nothing else — in
/// particular not on the authority the request merely carried, where that is not the socket
/// this process is served on.
fn assert_announced_on(case: &str, member: &str, expected_base: &str, dialled: &str, url: &str) {
    assert_is_a_url(case, member, url);
    assert!(
        url.starts_with(&format!("{expected_base}/")),
        "case {case} {member}: expected the address published on {expected_base:?}, got {url:?}"
    );
    if expected_base != format!("https://{dialled}") {
        let announced = url
            .split_once("://")
            .map_or(url, |(_scheme, rest)| rest)
            .split('/')
            .next()
            .unwrap_or_default();
        assert_ne!(
            announced, dialled,
            "case {case} {member}: the authority the request carried names no socket this \
             process serves, so it may not be published, got {url:?}"
        );
    }
}

/// RULE: the addresses a `SearchListings` page publishes — every summary's `logoThumbnailUrl`
/// and the `sourceUrl` of every summary whose review source keeps a reviews page — are
/// announced on the socket this process is served on, carry the record's own path, and are
/// served HERE on that same authority.
///
/// Rows: every authority of [`DIALLED`], over every summary of a whole catalog page.
#[tokio::test]
async fn a_search_page_publishes_its_addresses_on_the_socket_this_process_is_served_on() {
    let (source_id, _source_name) = REVIEW_SOURCE;
    let router = router_served_at(SERVED_BASE);

    for (dialled, expected_base) in DIALLED {
        let body = dialled_at(
            &router,
            dialled,
            "searchListings",
            &json!({ "maxResults": MAX_PAGE }),
        )
        .await;
        let summaries = array(&body, "listingSummaries");
        assert!(
            !summaries.is_empty(),
            "case dialled {dialled}: the seeded catalog must report listings, got {body}"
        );
        let mut fetched = 0_usize;
        let mut fetched_pages = 0_usize;

        for summary in &summaries {
            let listing_id = text(summary, "listingId");
            let case = format!("dialled {dialled} {listing_id}");
            let logo = text(summary, "logoThumbnailUrl");
            assert_announced_on(&case, "logoThumbnailUrl", expected_base, dialled, &logo);
            // The path is the record's own media address on that authority, so only the
            // authority moves with the request.
            let media_id = LogoMedia::of_listing(&listing_id).media_id().to_string();
            assert_eq!(
                logo,
                logo_url(expected_base, &media_id),
                "case {case}: expected this listing's own logo address on {expected_base:?}, got \
                 {logo:?}"
            );

            let reviews = review_source(summary).and_then(|(_source, url)| url);
            if let Some(url) = &reviews {
                assert_announced_on(&case, "sourceUrl", expected_base, dialled, url);
                assert_eq!(
                    *url,
                    reviews_page_url(expected_base, &reviews_list_id(&listing_id, source_id)),
                    "case {case}: expected this listing's own reviews page on {expected_base:?}, \
                     got {url:?}"
                );
            }

            if fetched >= FETCHED_PER_ROW {
                continue;
            }
            fetched += 1;
            // And the address announced is served at the authority it was announced on: the
            // fetch states exactly the authority of the published URL.
            let image = fetch(&router, "GET", &logo).await;
            assert_eq!(
                image.status, 200,
                "case {case}: expected the logo announced at {logo:?} to be served on that \
                 authority, got {} ({:?})",
                image.status,
                image.as_text()
            );
            assert!(
                image.body.starts_with(&PNG_SIGNATURE),
                "case {case}: expected PNG bytes at {logo:?}, got {} bytes starting {:?}",
                image.body.len(),
                image.body.iter().take(8).collect::<Vec<_>>()
            );
            let Some(url) = &reviews else {
                continue;
            };
            let page = fetch(&router, "GET", url).await;
            assert_eq!(
                page.status, 200,
                "case {case}: expected the reviews page announced at {url:?} to be served on \
                 that authority, got {} ({:?})",
                page.status,
                page.as_text()
            );
            assert!(
                page.as_text().contains(listing_id.as_str()),
                "case {case}: expected the page to restate the listing it is the reviews of, got \
                 {:?}",
                page.as_text()
            );
            fetched_pages += 1;
        }
        assert!(
            fetched > 0 && fetched_pages > 0,
            "case dialled {dialled}: the row must have fetched a logo and a reviews page, got \
             {fetched} logos and {fetched_pages} pages"
        );
    }
}

/// The first summary of `summaries` whose fulfillment option summaries name a SaaS option —
/// `FulfillmentOptionSummary$fulfillmentOptionType` is "The machine-readable type of the
/// fulfillment option, such as `SAAS`" — which is the record whose `ListFulfillmentOptions`
/// read publishes a buyer-facing page.
fn first_saas_listing(summaries: &[Value]) -> Value {
    summaries
        .iter()
        .find(|summary| {
            array(summary, "fulfillmentOptionSummaries")
                .iter()
                .any(|option| text(option, "fulfillmentOptionType") == "SAAS")
        })
        .cloned()
        .expect("the seeded catalog holds a listing fulfilled as SaaS")
}

/// RULE: the authority is a property of this PROCESS, not of the operation, so every
/// operation of this service that publishes a fetchable address publishes it on the same
/// socket — the one this process is served on.
///
/// Rows: the two authorities a client can state that differ in kind — the served socket
/// itself, and a port this process does not serve — each walked through the whole drill-down
/// chain `SearchListings` -> `GetListing` -> `GetProduct` -> `ListFulfillmentOptions`.
#[tokio::test]
async fn every_operation_publishes_its_addresses_on_that_same_socket() {
    let router = router_served_at(SERVED_BASE);

    for (dialled, expected_base) in [
        ("127.0.0.1:9297", "https://127.0.0.1:9297"),
        ("127.0.0.1:38377", SERVED_BASE),
    ] {
        let page = dialled_at(
            &router,
            dialled,
            "searchListings",
            &json!({ "maxResults": MAX_PAGE }),
        )
        .await;
        let summary = first_saas_listing(&array(&page, "listingSummaries"));
        let listing_id = text(&summary, "listingId");
        let case = format!("dialled {dialled} {listing_id}");
        assert_announced_on(
            &case,
            "SearchListings logoThumbnailUrl",
            expected_base,
            dialled,
            &text(&summary, "logoThumbnailUrl"),
        );

        let listing = dialled_at(
            &router,
            dialled,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        let announced = text(&listing, "logoThumbnailUrl");
        assert_announced_on(
            &case,
            "GetListing logoThumbnailUrl",
            expected_base,
            dialled,
            &announced,
        );
        assert_eq!(
            announced,
            text(&summary, "logoThumbnailUrl"),
            "case {case}: one record's logo is one address, whichever operation reports it"
        );
        if let Some(url) = review_source(&listing).and_then(|(_source, url)| url) {
            assert_announced_on(&case, "GetListing sourceUrl", expected_base, dialled, &url);
        }

        let mut products = 0_usize;
        let mut options = 0_usize;
        for entity in array(&listing, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            let body = dialled_at(
                &router,
                dialled,
                "getProduct",
                &json!({ "productId": product_id.clone() }),
            )
            .await;
            assert_announced_on(
                &format!("{case}/{product_id}"),
                "GetProduct logoThumbnailUrl",
                expected_base,
                dialled,
                &text(&body, "logoThumbnailUrl"),
            );
            products += 1;

            let fulfillment = dialled_at(
                &router,
                dialled,
                "listFulfillmentOptions",
                &json!({ "productId": product_id.clone(), "maxResults": 50 }),
            )
            .await;
            for option in array(&fulfillment, "fulfillmentOptions") {
                let Some(saas) = option.get("saasFulfillmentOption") else {
                    continue;
                };
                for member in ["fulfillmentUrl", "launchUrl"] {
                    let Some(url) = saas.get(member).and_then(Value::as_str) else {
                        continue;
                    };
                    assert_announced_on(
                        &format!("{case}/{product_id}"),
                        member,
                        expected_base,
                        dialled,
                        url,
                    );
                    let page = fetch(&router, "GET", url).await;
                    assert_eq!(
                        page.status, 200,
                        "case {case}/{product_id} {member}: expected the page announced at \
                         {url:?} to be served on that authority, got {} ({:?})",
                        page.status,
                        page.as_text()
                    );
                    options += 1;
                }
            }
        }
        assert!(
            products > 0,
            "case {case}: a catalog listing must associate at least one product"
        );
        assert!(
            options > 0,
            "case {case}: the walk must cover a fulfillment option that publishes a page"
        );
    }
}
