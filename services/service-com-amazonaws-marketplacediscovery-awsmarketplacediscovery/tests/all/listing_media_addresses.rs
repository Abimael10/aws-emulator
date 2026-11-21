// SPDX-License-Identifier: CC0-1.0

//! The addresses a CATALOG RECORD publishes its media at, end to end: the logo thumbnail of a
//! listing or product, and the reviews page a review source keeps for a listing.
//!
//! The rule under test: a URL this service hands a client to FETCH is an address THIS emulator
//! serves — the endpoint the request reached it at, followed by a path minted from the
//! record's own stored media — and an OPTIONAL one is announced only for a record that holds
//! the media it names. So:
//!
//! * `logoThumbnailUrl` (required) is the listing's own logo address on this emulator's
//!   endpoint, one per listing, carrying nothing of the text the listing is displayed by, and
//!   a fetch of it is answered here with the image;
//! * `ReviewSourceSummary$sourceUrl` (optional) is present only for a listing the source keeps
//!   a reviews page for, is that listing's own page, and a fetch of it is answered here with
//!   the page; for a listing the source keeps none for, the member is absent and the address it
//!   would have is served as nothing.
//!
//! Every suite drives real HTTP requests through the `MultiServiceRouter`, so the protocol, the
//! routing table and the handlers are all exercised, and the published addresses are fetched
//! the way a browser holding one does: anonymously, over the path alone.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::state::{
    logo_url, reviews_list_id, reviews_page_url, LogoMedia, REVIEW_SOURCE,
};
use service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::EmulatedService;

/// The model's `POST /2026-02-05/<op>` route prefix (`OPERATIONS.md`).
const PREFIX: &str = "http://discovery-marketplace.us-east-1.amazonaws.com/2026-02-05";

/// A SigV4 `Authorization` header whose credential scope names this service's signing name,
/// which is what the router's `can_handle` routes on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260205/us-east-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The endpoint bases this emulator can report as its own: the forms a data port is published
/// at, plus the base of an emulator served on no port at all (embedded in another process, or
/// a test). One row each, because an address a record publishes has to follow whichever one the
/// request that read it reached this emulator at.
pub(crate) const ENDPOINT_BASES: [&str; 4] = [
    "https://127.0.0.1:9297",
    "https://localhost:39355",
    "https://[::1]:8443",
    smithy_server_runtime::UNSERVED_URL_BASE,
];

/// Hosts this emulator does not answer on: the zones AWS publishes its own endpoints in, and
/// the domain reserved for documentation examples. A published address may name none of them,
/// because nothing serves a fetch of one.
pub(crate) const UNSERVED_HOSTS: [&str; 4] =
    ["example.com", ".amazonaws.com", ".api.aws", ".on.aws"];

/// The first eight bytes of every PNG file (the PNG signature), which is what the logo address
/// has to serve for a client that renders the image to get one.
pub(crate) const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// How many listings the per-record round trips below are run over.
///
/// The URL rule is asserted over EVERY summary of a whole page; the round trips that read the
/// same record back through `GetListing` and `GetProduct` take a sample of them, which keeps
/// the suite's call count bounded while still spanning the catalog's tiers (its hand-written
/// records come first in listing-id order, its derived ones after).
const SAMPLED_LISTINGS: usize = 8;

fn router() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing, so request authentication is off for this router only.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A router whose `OperationContext::endpoint` is `base`, which is the address the media a
/// record publishes has to be announced on.
pub(crate) fn router_served_at(base: &str) -> MultiServiceRouter {
    let mut router = router();
    router.set_url_base(
        smithy_server_runtime::EndpointUrl::parse(base).expect("a base URL the router accepts"),
    );
    router
}

/// Call one operation and return its HTTP status and JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, input: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{PREFIX}/{operation}"))
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(input.to_string()))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, body)
}

/// Call one operation, requiring HTTP 200.
async fn ok(router: &MultiServiceRouter, operation: &str, input: &Value) -> Value {
    let (status, body) = call(router, operation, input).await;
    assert_eq!(
        status, 200,
        "case {operation} {input}: expected HTTP 200, got {status} with body {body}"
    );
    body
}

fn array(body: &Value, member: &str) -> Vec<Value> {
    body.get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn text(value: &Value, member: &str) -> String {
    value
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One document a client FETCHES: the status, the content type and the bytes.
pub(crate) struct Fetched {
    pub(crate) status: u16,
    pub(crate) content_type: String,
    pub(crate) body: Vec<u8>,
}

impl Fetched {
    /// The document as text, for a page.
    pub(crate) fn as_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Fetch `url` the way a browser holding a published address does: no credential and no target
/// header, just the address the record announced.
pub(crate) async fn fetch(router: &MultiServiceRouter, method: &str, url: &str) -> Fetched {
    let (host, path) = split_url(url);
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("build the request");
    let response = router.dispatch(request).await;
    Fetched {
        status: response.status().as_u16(),
        content_type: response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string(),
        body: response.body().to_vec(),
    }
}

/// The authority and the path of an absolute URL, which is how a client addresses it.
fn split_url(url: &str) -> (String, String) {
    let without_scheme = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), "/".to_string()),
    }
}

/// Whether `id` has the shape of an identifier this catalog ISSUES: the name-based
/// (version 5) UUID form `8-4-4-4-12` in lowercase hex, which is how the catalog mints every
/// opaque handle it hands out (`state::category_id` for a `categoryId`, `LogoMedia` for a
/// logo's media id, `reviews_list_id` for a reviews list).
///
/// An identifier of that shape carries none of the text the record is displayed by, which is
/// what makes a published address an opaque handle rather than a form of a name.
fn is_issued_identifier(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && id
            .chars()
            .all(|c| c == '-' || c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// One page of the catalog's listing summaries, as large a page as `maxResults` allows.
async fn catalog_page(router: &MultiServiceRouter) -> Vec<Value> {
    let body = ok(router, "searchListings", &json!({ "maxResults": 100 })).await;
    let summaries = array(&body, "listingSummaries");
    assert!(
        !summaries.is_empty(),
        "case setup: the seeded catalog must hold listings, got {body}"
    );
    summaries
}

/// The one review source summary of a listing's review summary, with the `sourceUrl` it
/// carries, if any.
pub(crate) fn review_source(body: &Value) -> Option<(Value, Option<String>)> {
    let summary = body.get("reviewSummary")?;
    let sources = array(summary, "reviewSourceSummaries");
    let source = sources.first()?.clone();
    let source_url = source
        .get("sourceUrl")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Some((source, source_url))
}

/// RULE: `logoThumbnailUrl` is the address THIS emulator serves that record's logo image at —
/// the endpoint the request reached it at, followed by the path of the media record the catalog
/// holds for the listing — and a fetch of it is answered here with a PNG.
///
/// Rows: every summary of a whole catalog page (hand-written records and derived ones, across
/// every tier), each of the endpoint bases this emulator can be reached at.
#[tokio::test]
async fn every_listing_announces_its_logo_on_the_endpoint_the_request_reached_this_emulator_at() {
    for base in ENDPOINT_BASES {
        let router = router_served_at(base);
        let mut addresses: BTreeSet<String> = BTreeSet::new();
        for summary in catalog_page(&router).await {
            let listing_id = text(&summary, "listingId");
            let published = text(&summary, "logoThumbnailUrl");

            // The address is the one the record's own media id is served at, on the endpoint
            // this request arrived on.
            let media_id = LogoMedia::of_listing(&listing_id).media_id().to_string();
            assert_eq!(
                published,
                logo_url(base, &media_id),
                "case {base:?} {listing_id}: expected the logo announced at this emulator's own \
                 endpoint, got {published:?}"
            );
            for host in UNSERVED_HOSTS {
                assert!(
                    !published.contains(host),
                    "case {base:?} {listing_id}: a published address must name no host this \
                     emulator does not serve, got {published:?}"
                );
            }
            // The part of the address that is the RECORD's — the media id and the fixed file
            // name — carries an identifier of the shape this catalog issues and none of the
            // text the listing is displayed by, so a client persisting it holds an
            // identifier rather than a form of the listing's name (the defect this replaces
            // addressed `…/logos/<name slug>.png`).
            let record_part = format!("{media_id}/logo.png");
            assert!(
                is_issued_identifier(&media_id),
                "case {base:?} {listing_id}: the media id must be an identifier of the shape \
                 this catalog issues, got {media_id:?}"
            );
            for word in text(&summary, "listingName").to_lowercase().split(' ') {
                assert!(
                    word.len() < 4 || !record_part.contains(word),
                    "case {base:?} {listing_id}: the address's own part must carry nothing of \
                     the listing's name, got {record_part:?} for {word:?}"
                );
            }
            assert!(
                addresses.insert(published.clone()),
                "case {base:?} {listing_id}: every listing has a logo address of its own, got \
                 {published:?} twice"
            );

            // And the image is served there.
            let image = fetch(&router, "GET", &published).await;
            assert_eq!(
                image.status, 200,
                "case {base:?} {listing_id}: expected {published:?} to be served here, got {} \
                 ({:?})",
                image.status,
                image.as_text()
            );
            assert_eq!(
                image.content_type, "image/png",
                "case {base:?} {listing_id}: expected the logo served as a PNG, got {:?}",
                image.content_type
            );
            assert!(
                image.body.starts_with(&PNG_SIGNATURE),
                "case {base:?} {listing_id}: expected PNG bytes at {published:?}, got {} bytes \
                 starting {:?}",
                image.body.len(),
                image.body.iter().take(8).collect::<Vec<_>>()
            );

            // What a client asks when it only wants to know the image is there.
            let head = fetch(&router, "HEAD", &published).await;
            assert_eq!(
                (head.status, head.body.is_empty()),
                (200, true),
                "case {base:?} {listing_id}: expected a HEAD of {published:?} to answer 200 with \
                 no body, got {} ({} bytes)",
                head.status,
                head.body.len()
            );
        }
    }
}

/// RULE: the logo of a record is ONE image at ONE address, so every operation that reports a
/// listing or its products announces the same address for it, read back off the same record.
///
/// Rows: a sample of listings spanning the catalog, each read through `SearchListings`,
/// `GetListing` and the `GetProduct` of every product it associates.
#[tokio::test]
async fn a_listings_logo_address_is_the_same_one_every_operation_announces() {
    let base = ENDPOINT_BASES[0];
    let router = router_served_at(base);
    let page = catalog_page(&router).await;
    let sampled: Vec<Value> = page
        .iter()
        .step_by((page.len() / SAMPLED_LISTINGS).max(1))
        .cloned()
        .collect();

    for summary in sampled {
        let listing_id = text(&summary, "listingId");
        let announced = text(&summary, "logoThumbnailUrl");

        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        assert_eq!(
            text(&listing, "logoThumbnailUrl"),
            announced,
            "case {listing_id}: GetListing must announce the logo address SearchListings does, \
             got {listing}"
        );

        let mut products = 0_usize;
        for entity in array(&listing, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            let body = ok(
                &router,
                "getProduct",
                &json!({ "productId": product_id.clone() }),
            )
            .await;
            assert_eq!(
                text(&body, "logoThumbnailUrl"),
                announced,
                "case {listing_id}/{product_id}: a product's logo is its listing's logo, at the \
                 same address, got {body}"
            );
            products += 1;
        }
        assert!(
            products > 0,
            "case {listing_id}: a catalog listing must associate at least one product"
        );
    }
}

/// RULE: `sourceUrl` is announced only for a listing its review source keeps a reviews page
/// for, is THAT listing's own page on this emulator's endpoint, and a fetch of it is answered
/// here with the page; where the record keeps none, the member is absent and the address it
/// would have serves nothing.
///
/// Rows: every summary of a whole catalog page, which holds records both with and without a
/// reviews page, over each endpoint base.
#[tokio::test]
async fn a_reviews_page_is_announced_only_where_the_record_keeps_one() {
    let (source_id, source_name) = REVIEW_SOURCE;
    for base in ENDPOINT_BASES {
        let router = router_served_at(base);
        let mut announced: BTreeSet<String> = BTreeSet::new();
        let mut withheld = 0_usize;
        for summary in catalog_page(&router).await {
            let listing_id = text(&summary, "listingId");
            let (source, source_url) = review_source(&summary).unwrap_or_else(|| {
                panic!("case {base:?} {listing_id}: a listing summary carries a reviewSummary")
            });
            assert_eq!(
                text(&source, "sourceId"),
                source_id,
                "case {base:?} {listing_id}: the review source must be the one the model \
                 declares, got {source}"
            );
            assert_eq!(
                text(&source, "sourceName"),
                source_name,
                "case {base:?} {listing_id}: expected the source's name, got {source}"
            );

            // The page this listing's reviews would be read at, whether or not the record keeps
            // one: the reviews list that source keeps for THIS listing.
            let page_url = reviews_page_url(base, &reviews_list_id(&listing_id, source_id));
            let Some(published) = source_url else {
                let page = fetch(&router, "GET", &page_url).await;
                assert_eq!(
                    page.status, 404,
                    "case {base:?} {listing_id}: a reviews page no record keeps must be served \
                     as nothing, got {} ({:?})",
                    page.status,
                    page.as_text()
                );
                assert!(
                    !page.as_text().contains(&listing_id),
                    "case {base:?} {listing_id}: a page nothing stands behind must restate no \
                     record, got {:?}",
                    page.as_text()
                );
                withheld += 1;
                continue;
            };
            assert_eq!(
                published, page_url,
                "case {base:?} {listing_id}: expected this listing's own reviews page on this \
                 emulator's endpoint, got {published:?}"
            );
            for host in UNSERVED_HOSTS {
                assert!(
                    !published.contains(host),
                    "case {base:?} {listing_id}: a published address must name no host this \
                     emulator does not serve, got {published:?}"
                );
            }
            assert!(
                announced.insert(published.clone()),
                "case {base:?} {listing_id}: a reviews page is one listing's, got {published:?} \
                 announced for a second listing"
            );

            let page = fetch(&router, "GET", &published).await;
            assert_eq!(
                page.status, 200,
                "case {base:?} {listing_id}: expected {published:?} to be served here, got {} \
                 ({:?})",
                page.status,
                page.as_text()
            );
            assert!(
                page.content_type.starts_with("text/html"),
                "case {base:?} {listing_id}: expected a page a client can open, got content type \
                 {:?}",
                page.content_type
            );
            // The page restates the record the summary came from, so the two cannot disagree.
            for stated in [
                listing_id.clone(),
                text(&summary, "listingName"),
                text(&source, "averageRating"),
                source
                    .get("totalReviews")
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ] {
                assert!(
                    page.as_text().contains(&stated),
                    "case {base:?} {listing_id}: expected the page to restate {stated:?} off the \
                     record, got {:?}",
                    page.as_text()
                );
            }
        }

        assert!(
            !announced.is_empty() && withheld > 0,
            "case {base:?}: the catalog must hold listings whose source keeps a reviews page and \
             listings whose source keeps none, so the presence of sourceUrl carries information, \
             got {} announced and {withheld} withheld",
            announced.len()
        );
    }
}

/// RULE: a listing's reviews summary is one record, so `GetListing` and `SearchListings` report
/// the same ratings and announce the same reviews page — present or absent alike.
#[tokio::test]
async fn get_listing_and_search_listings_report_one_reviews_record() {
    let base = ENDPOINT_BASES[1];
    let router = router_served_at(base);
    let page = catalog_page(&router).await;
    let sampled: Vec<Value> = page
        .iter()
        .step_by((page.len() / SAMPLED_LISTINGS).max(1))
        .cloned()
        .collect();

    let mut with_page = 0_usize;
    let mut without_page = 0_usize;
    for summary in sampled {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        let (searched, searched_url) = review_source(&summary).expect("a summary's review source");
        let (read, read_url) = review_source(&listing).expect("a listing's review source");
        assert_eq!(
            searched, read,
            "case {listing_id}: the two operations report one review record, got {searched} \
             against {read}"
        );
        assert_eq!(
            searched_url, read_url,
            "case {listing_id}: the two operations announce one reviews page, got \
             {searched_url:?} against {read_url:?}"
        );
        if read_url.is_some() {
            with_page += 1;
        } else {
            without_page += 1;
        }
    }
    assert!(
        with_page > 0 && without_page > 0,
        "case the sampled listings: both a listing whose source keeps a reviews page and one \
         whose source keeps none must be covered, got {with_page} with and {without_page} without"
    );
}

/// RULE: only an address this service PUBLISHED is served: a media id or reviews list this
/// catalog holds nothing for, a path below one (an image and a page are each one document,
/// not a path space) and a method other than a fetch are all answered as nothing.
#[tokio::test]
async fn only_a_published_address_is_served() {
    let base = smithy_server_runtime::UNSERVED_URL_BASE;
    let router = router_served_at(base);
    let summary = catalog_page(&router).await.remove(0);
    let listing_id = text(&summary, "listingId");
    let media_id = LogoMedia::of_listing(&listing_id).media_id().to_string();
    let published = logo_url(base, &media_id);

    // One row per way an address can fail to name media this service published.
    let cases: Vec<(&str, String)> = vec![
        (
            "a media id this catalog holds nothing for",
            logo_url(base, "00000000-0000-0000-0000-000000000000"),
        ),
        (
            "a reviews list this catalog keeps for no listing",
            reviews_page_url(base, "00000000-0000-0000-0000-000000000000"),
        ),
        (
            "a reviews list minted for another source",
            reviews_page_url(base, &reviews_list_id(&listing_id, "OTHER_SOURCE")),
        ),
        (
            "a path below the image, which is one document",
            format!("{published}/large"),
        ),
        ("no media id named at all", logo_url(base, "")),
    ];
    for (case, address) in cases {
        let fetched = fetch(&router, "GET", &address).await;
        assert_ne!(
            fetched.status, 200,
            "case {case}: {address:?} names nothing this service published, got {} ({:?})",
            fetched.status,
            fetched.as_text()
        );
        assert!(
            !fetched.as_text().contains(&listing_id),
            "case {case}: an answer for {address:?} must restate no record, got {:?}",
            fetched.as_text()
        );
    }

    // A method that is not a fetch of the image is not this route's either.
    let posted = fetch(&router, "POST", &published).await;
    assert_ne!(
        posted.status, 200,
        "case POST of a published logo address: expected it not to be served, got {} ({:?})",
        posted.status,
        posted.as_text()
    );

    // And the address this service DID publish is still served, so the answers above are the
    // shape of the address rather than the route being unreachable.
    let image = fetch(&router, "GET", &published).await;
    assert_eq!(
        image.status, 200,
        "case the published address {published:?}: expected it to be served here, got {}",
        image.status
    );
}
