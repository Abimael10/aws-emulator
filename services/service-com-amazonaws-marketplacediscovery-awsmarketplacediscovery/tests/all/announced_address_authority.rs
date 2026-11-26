// SPDX-License-Identifier: CC0-1.0

//! The AUTHORITY every address this service publishes for a client to FETCH is announced on.
//!
//! The rule under test: an address a response carries for a client to dereference —
//! `ListingSummary$logoThumbnailUrl` and `GetListing`/`GetProduct`'s own
//! (`URL`, `@required`, `@length(1..2048)`, `@pattern ^https?://.*$`),
//! `ReviewSourceSummary$sourceUrl` (the same `URL`, optional) and a SaaS fulfillment option's
//! `fulfillmentUrl` — is published on the authority the ANSWERING REQUEST ARRIVED ON, because
//! that authority is one that demonstrably reaches this process:
//!
//! * a client is handed its own spelling of the authority it dialled, so the name it resolved
//!   to get here is the name it dials again — including a port this process is not itself
//!   served on, which is how a published container port or a forwarding front end reaches it;
//! * a request that states an authority which cannot lead back here — one of the hosts AWS
//!   publishes its own endpoints at, or an authority naming no port where this process is not
//!   served on the one `https` implies — is answered with the address this process IS served
//!   at, since that is the only address of this process left to publish.
//!
//! [`crate::listing_media_addresses`] and [`crate::search_page_media_addresses`] pin which
//! RECORD each published address belongs to and that a fetch of it is served here; this suite
//! pins the one thing that is a property of the ADDRESSING rather than of the record — which
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
/// on directly, which is the address a published URL names when the request carries none that
/// can lead back here.
///
/// Shared with [`crate::search_page_announced_authority`], which drives the same rule over the
/// shapes of search page a client is served, so the two suites cannot disagree about which
/// socket this process is served on.
pub(crate) const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// How a client STATES the authority it reached this emulator at, and the base the addresses of
/// the response it gets have to be published on.
///
/// A request carries that authority in its `Host` header, in the authority of an absolute-form
/// request target (which is how HTTP/2 and an SDK-built request spell it), in both, or in
/// neither — so a row names each place separately.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dialled {
    /// What the row is, for the assertion messages.
    pub(crate) case: &'static str,
    /// The `Host` header the request carries.
    pub(crate) host_header: Option<&'static str>,
    /// The authority of the absolute-form request target the request carries.
    pub(crate) target_authority: Option<&'static str>,
    /// The scheme and authority every published address of the answer has to be on.
    pub(crate) expected_base: &'static str,
}

impl Dialled {
    /// A row that states one authority in BOTH places, which is how a client reaching a socket
    /// over the wire states it.
    const fn at(case: &'static str, authority: &'static str, expected_base: &'static str) -> Self {
        Self {
            case,
            host_header: Some(authority),
            target_authority: Some(authority),
            expected_base,
        }
    }

    /// The authority this row stated, for the reading that may not re-announce an authority a
    /// fetch cannot lead back here by, and `None` for a row that stated none.
    pub(crate) fn stated(self) -> Option<&'static str> {
        self.host_header.or(self.target_authority)
    }
}

/// The authorities a client can state, each with the base the addresses of the response it gets
/// have to be published on. The ONE table of them for this crate: every suite that drives the
/// announced-address rule walks these rows, so two suites cannot disagree about what a given
/// authority must be answered with.
///
/// The rows differ along what the rule generalises over: the served authority itself, another
/// spelling of that same socket, a port this process is not served on but was reached through
/// (the shape of the defect this pins), a loopback name on such a port, an authority naming no
/// port at all, a host AWS publishes its own endpoints at, one of those hosts carrying the
/// served port, an authority stated only in the request target, and a request that states no
/// authority at all.
pub(crate) const DIALLED: [Dialled; 9] = [
    Dialled::at("the served socket itself", "127.0.0.1:9297", SERVED_BASE),
    Dialled::at(
        "another spelling of the served socket",
        "localhost:9297",
        "https://localhost:9297",
    ),
    Dialled::at(
        "a port this process is not served on",
        "127.0.0.1:38377",
        "https://127.0.0.1:38377",
    ),
    Dialled::at(
        "a loopback name on a port this process is not served on",
        "localhost:39355",
        "https://localhost:39355",
    ),
    Dialled::at("an authority naming no port", "127.0.0.1", SERVED_BASE),
    Dialled::at(
        "an endpoint host AWS publishes its own services at",
        "discovery-marketplace.us-east-1.amazonaws.com",
        SERVED_BASE,
    ),
    Dialled::at(
        "one of those hosts carrying the served port",
        "discovery-marketplace.us-east-1.amazonaws.com:9297",
        SERVED_BASE,
    ),
    Dialled {
        case: "an authority stated only in the request target",
        host_header: None,
        target_authority: Some("127.0.0.1:41509"),
        expected_base: "https://127.0.0.1:41509",
    },
    Dialled {
        case: "no authority stated at all",
        host_header: None,
        target_authority: None,
        expected_base: SERVED_BASE,
    },
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
pub(crate) async fn dialled_at(
    router: &MultiServiceRouter,
    host: &str,
    operation: &str,
    input: &Value,
) -> Value {
    dialled_from(router, Some(host), Some(host), operation, input).await
}

/// [`dialled_at`] over the two places a request carries the authority it reached this emulator
/// at SEPARATELY: the `Host` header, and the authority of an absolute-form request target —
/// which is how HTTP/2 and an SDK-built request spell the same thing. A row that states
/// neither carries no authority at all.
pub(crate) async fn dialled_from(
    router: &MultiServiceRouter,
    host_header: Option<&str>,
    target_authority: Option<&str>,
    operation: &str,
    input: &Value,
) -> Value {
    let target = match target_authority {
        Some(authority) => format!("https://{authority}/2026-02-05/{operation}"),
        None => format!("/2026-02-05/{operation}"),
    };
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(target)
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    if let Some(host) = host_header {
        builder = builder.header("host", host);
    }
    let request = builder
        .body(bytes::Bytes::from(input.to_string()))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "case dialled {host_header:?}/{target_authority:?} {operation}: expected HTTP 200, got \
         {status} ({:?})",
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
/// particular not on an authority the request merely carried that cannot lead a fetch back
/// here.
pub(crate) fn assert_announced_on(
    case: &str,
    member: &str,
    expected_base: &str,
    dialled: Option<&str>,
    url: &str,
) {
    assert_is_a_url(case, member, url);
    assert!(
        url.starts_with(&format!("{expected_base}/")),
        "case {case} {member}: expected the address published on {expected_base:?}, got {url:?}"
    );
    let Some(dialled) = dialled else {
        return;
    };
    if expected_base != format!("https://{dialled}") {
        let announced = url
            .split_once("://")
            .map_or(url, |(_scheme, rest)| rest)
            .split('/')
            .next()
            .unwrap_or_default();
        assert_ne!(
            announced, dialled,
            "case {case} {member}: the authority the request carried cannot lead a fetch back \
             here, so it may not be published, got {url:?}"
        );
    }
}

/// RULE, over ONE summary of an answered page: the summary announces its own listing's logo
/// address, and the reviews page of a review source that keeps one, on the authority the
/// request arrived on, with the RECORD's own path after it — and, when `fetch_documents`, a
/// fetch stating exactly the announced authority is answered HERE with the image resp. the
/// page.
///
/// Returns whether the summary announced a reviews page, so a caller can require its table to
/// have covered a summary that keeps one.
///
/// The one reading of what a summary announces, shared with
/// [`crate::search_page_announced_authority`]: both suites assert the same thing about a
/// summary and differ only in which pages they read.
pub(crate) async fn assert_summary_addresses(
    router: &MultiServiceRouter,
    case: &str,
    expected_base: &str,
    dialled: Option<&str>,
    summary: &Value,
    fetch_documents: bool,
) -> bool {
    let (source_id, _source_name) = REVIEW_SOURCE;
    let listing_id = text(summary, "listingId");
    let logo = text(summary, "logoThumbnailUrl");
    assert_announced_on(case, "logoThumbnailUrl", expected_base, dialled, &logo);
    // The path is the record's own media address on that authority, so only the authority
    // moves with the request.
    let media_id = LogoMedia::of_listing(&listing_id).media_id().to_string();
    assert_eq!(
        logo,
        logo_url(expected_base, &media_id),
        "case {case}: expected this listing's own logo address on {expected_base:?}, got {logo:?}"
    );

    let reviews = review_source(summary).and_then(|(_source, url)| url);
    if let Some(url) = &reviews {
        assert_announced_on(case, "sourceUrl", expected_base, dialled, url);
        assert_eq!(
            *url,
            reviews_page_url(expected_base, &reviews_list_id(&listing_id, source_id)),
            "case {case}: expected this listing's own reviews page on {expected_base:?}, got \
             {url:?}"
        );
    }
    if !fetch_documents {
        return reviews.is_some();
    }

    // And the address announced is served at the authority it was announced on: the fetch
    // states exactly the authority of the published URL.
    let image = fetch(router, "GET", &logo).await;
    assert_eq!(
        image.status, 200,
        "case {case}: expected the logo announced at {logo:?} to be served on that authority, got \
         {} ({:?})",
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
        return false;
    };
    let page = fetch(router, "GET", url).await;
    assert_eq!(
        page.status, 200,
        "case {case}: expected the reviews page announced at {url:?} to be served on that \
         authority, got {} ({:?})",
        page.status,
        page.as_text()
    );
    assert!(
        page.as_text().contains(listing_id.as_str()),
        "case {case}: expected the page to restate the listing it is the reviews of, got {:?}",
        page.as_text()
    );
    true
}

/// RULE: the addresses a `SearchListings` page publishes — every summary's `logoThumbnailUrl`
/// and the `sourceUrl` of every summary whose review source keeps a reviews page — are
/// announced on the authority the request arrived on, carry the record's own path, and are
/// served HERE on that same authority.
///
/// Rows: every authority of [`DIALLED`], over every summary of a whole catalog page.
#[tokio::test]
async fn a_search_page_publishes_its_addresses_on_the_authority_the_request_arrived_on() {
    let router = router_served_at(SERVED_BASE);

    for dialled in DIALLED {
        let body = dialled_from(
            &router,
            dialled.host_header,
            dialled.target_authority,
            "searchListings",
            &json!({ "maxResults": MAX_PAGE }),
        )
        .await;
        let summaries = array(&body, "listingSummaries");
        assert!(
            !summaries.is_empty(),
            "case dialled {:?}: the seeded catalog must report listings, got {body}",
            dialled.case
        );
        let mut fetched = 0_usize;
        let mut fetched_pages = 0_usize;

        for summary in &summaries {
            let listing_id = text(summary, "listingId");
            let case = format!("dialled {:?} {listing_id}", dialled.case);
            let fetch_documents = fetched < FETCHED_PER_ROW;
            let announced_reviews = assert_summary_addresses(
                &router,
                &case,
                dialled.expected_base,
                dialled.stated(),
                summary,
                fetch_documents,
            )
            .await;
            if fetch_documents {
                fetched += 1;
                if announced_reviews {
                    fetched_pages += 1;
                }
            }
        }
        assert!(
            fetched > 0 && fetched_pages > 0,
            "case dialled {:?}: the row must have fetched a logo and a reviews page, got \
             {fetched} logos and {fetched_pages} pages",
            dialled.case
        );
    }
}

/// The first summary of `summaries` whose fulfillment option summaries name a SaaS option —
/// `FulfillmentOptionSummary$fulfillmentOptionType` is "The machine-readable type of the
/// fulfillment option, such as `SAAS`" — which is the record whose `ListFulfillmentOptions`
/// read publishes a buyer-facing page.
pub(crate) fn first_saas_listing(summaries: &[Value]) -> Value {
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

/// RULE: the authority is a property of the ADDRESSING, not of the operation, so every
/// operation of this service that publishes a fetchable address publishes it on the same
/// authority — the one the answering request arrived on.
///
/// Rows: the two authorities a client can state that differ in kind — the served socket
/// itself, and a port this process is not served on but was reached through — each walked
/// through the whole drill-down chain `SearchListings` -> `GetListing` -> `GetProduct` ->
/// `ListFulfillmentOptions`.
#[tokio::test]
async fn every_operation_publishes_its_addresses_on_that_same_authority() {
    let router = router_served_at(SERVED_BASE);

    for (dialled, expected_base) in [
        ("127.0.0.1:9297", "https://127.0.0.1:9297"),
        ("127.0.0.1:38377", "https://127.0.0.1:38377"),
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
            Some(dialled),
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
            Some(dialled),
            &announced,
        );
        assert_eq!(
            announced,
            text(&summary, "logoThumbnailUrl"),
            "case {case}: one record's logo is one address, whichever operation reports it"
        );
        if let Some(url) = review_source(&listing).and_then(|(_source, url)| url) {
            assert_announced_on(
                &case,
                "GetListing sourceUrl",
                expected_base,
                Some(dialled),
                &url,
            );
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
                Some(dialled),
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
                        Some(dialled),
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
