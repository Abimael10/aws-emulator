// SPDX-License-Identifier: CC0-1.0

//! A product found the documented way — search the catalog, then drill into an associated
//! product — offers every route its record is delivered through, so the one-to-many
//! relation `ListFulfillmentOptions` pages over is reachable from a search.
//!
//! The rule under test: a product carries one `FulfillmentOption` per route it is
//! delivered through, which for a product offered through several routes is several, and
//! `ListFulfillmentOptions` is `@paginated {pageSize: maxResults, items: fulfillmentOptions,
//! outputToken: nextToken}` over them — so a scan of the first page of a default
//! `SearchListings` reaches a product with 2+ options, a page as large as the option list
//! carries no `nextToken`, and a smaller page carries exactly `maxResults` options plus a
//! `nextToken` that walks the rest without repeating or skipping one.
//!
//! The suite drives real HTTP requests through the `MultiServiceRouter`, so the protocol,
//! the routing table, input validation and the handlers are all exercised.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::EmulatedService;

/// The model's `POST /2026-02-05/<op>` route prefix (`OPERATIONS.md`).
const PREFIX: &str = "http://discovery-marketplace.us-east-1.amazonaws.com/2026-02-05";

/// A SigV4 `Authorization` header whose credential scope names this service's signing
/// name, which is what the router's `can_handle` routes on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260205/us-east-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The largest page `ListFulfillmentOptions` serves: `maxResults` has `@range(min: 1, max:
/// 50)` (`smithy_query(query="operation",operation="ListFulfillmentOptions")`), so a page
/// of this size holds every option of a product the catalog seeds.
const MAX_OPTIONS_PAGE: usize = 50;

fn router() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing, so request authentication is off for this router only.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Call one operation, requiring HTTP 200.
async fn ok(router: &MultiServiceRouter, operation: &str, input: &Value) -> Value {
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

/// Every distinct product id a page of `SearchListings` associates, in the order the page
/// reports them: what a client that discovers products the documented way can reach.
async fn products_of_page(router: &MultiServiceRouter, search: &Value) -> Vec<String> {
    let page = ok(router, "searchListings", search).await;
    let summaries = array(&page, "listingSummaries");
    assert!(
        !summaries.is_empty(),
        "case {search}: a default search must match listings, got {page}"
    );
    let mut product_ids: Vec<String> = Vec::new();
    for summary in summaries {
        for entity in array(&summary, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            if !product_id.is_empty() && !product_ids.contains(&product_id) {
                product_ids.push(product_id);
            }
        }
    }
    assert!(
        !product_ids.is_empty(),
        "case {search}: the listings of a search page must associate products"
    );
    product_ids
}

/// Every fulfillment option of `product_id`, read in one page as large as the operation
/// serves — which therefore carries no continuation token.
async fn all_options(router: &MultiServiceRouter, product_id: &str) -> Vec<Value> {
    let body = ok(
        router,
        "listFulfillmentOptions",
        &json!({ "productId": product_id, "maxResults": MAX_OPTIONS_PAGE }),
    )
    .await;
    let options = array(&body, "fulfillmentOptions");
    assert!(
        options.len() <= MAX_OPTIONS_PAGE,
        "case {product_id}: a page of {MAX_OPTIONS_PAGE} must hold at most that many options, \
         got {}",
        options.len()
    );
    assert!(
        body.get("nextToken").is_none(),
        "case {product_id}: a page holding every option must carry no nextToken, got {body}"
    );
    options
}

/// How many options each product of a search page offers, paired with its id.
async fn option_counts(router: &MultiServiceRouter, search: &Value) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for product_id in products_of_page(router, search).await {
        let total = all_options(router, &product_id).await.len();
        counts.push((product_id, total));
    }
    counts
}

/// The searches a client makes when it scans the catalog without narrowing it: the modelled
/// defaults (`sortBy=RELEVANCE`, `sortOrder=DESCENDING`, `maxResults=25`) and pages of other
/// sizes and orderings of that same unfiltered scan.
///
/// `maxResults` has `@range(min: 1, max: 100)` and the sort members their modelled values
/// (`smithy_query(query="operation",operation="SearchListings")`); every row here carries no
/// `searchText` and no `filters`, so each one is a scan over the whole catalog.
fn default_scans() -> Vec<(&'static str, Value)> {
    vec![
        ("maxResults omitted, so the modelled default 25", json!({})),
        ("maxResults=5", json!({ "maxResults": 5 })),
        ("maxResults=10", json!({ "maxResults": 10 })),
        ("maxResults=20", json!({ "maxResults": 20 })),
        (
            "maxResults=20, sortOrder=ASCENDING",
            json!({ "maxResults": 20, "sortOrder": "ASCENDING" }),
        ),
        (
            "maxResults=20, sortBy=AVERAGE_CUSTOMER_RATING",
            json!({ "maxResults": 20, "sortBy": "AVERAGE_CUSTOMER_RATING" }),
        ),
        (
            "maxResults=20, sortBy=AVERAGE_CUSTOMER_RATING, sortOrder=ASCENDING",
            json!({
                "maxResults": 20,
                "sortBy": "AVERAGE_CUSTOMER_RATING",
                "sortOrder": "ASCENDING",
            }),
        ),
    ]
}

#[tokio::test]
async fn a_default_scan_reaches_a_product_offered_through_several_routes() {
    let router = router();

    // Rows: the unfiltered scans of `default_scans`, which differ in page size and in both
    // sort keys and directions — so the claim is about what a scan of the catalog reaches
    // rather than about one ordering of it.
    for (case, search) in default_scans() {
        let counts = option_counts(&router, &search).await;
        let multi: Vec<&(String, usize)> = counts.iter().filter(|(_, total)| *total >= 2).collect();
        assert!(
            !multi.is_empty(),
            "case {case}: a scan must reach a product offering 2+ fulfillment options, got \
             {counts:?}"
        );
        // Every product offers at least one route it is delivered through: a product the
        // catalog can be asked about is fulfilled somehow.
        for (product_id, total) in &counts {
            assert!(
                *total >= 1,
                "case {case} {product_id}: expected at least one fulfillment option, got {total}"
            );
        }
    }
}

#[tokio::test]
async fn a_default_scan_reaches_single_route_and_multi_route_products_alike() {
    let router = router();

    // A client branches on HOW MANY routes a product offers, so both shapes have to be
    // reachable from the same scan: a product with one option (no choice to render) and one
    // with several (a "choose how to deploy" step).
    let counts = option_counts(&router, &json!({ "maxResults": 20 })).await;
    assert!(
        counts.iter().any(|(_, total)| *total == 1),
        "case maxResults=20: a scan must reach a product offering exactly one fulfillment \
         option, got {counts:?}"
    );
    assert!(
        counts.iter().any(|(_, total)| *total >= 2),
        "case maxResults=20: a scan must reach a product offering 2+ fulfillment options, got \
         {counts:?}"
    );
}

#[tokio::test]
async fn max_results_truncates_a_reached_products_options_and_mints_a_token() {
    let router = router();
    let reachable = option_counts(&router, &json!({ "maxResults": 20 })).await;
    let multi: Vec<(String, usize)> = reachable
        .iter()
        .filter(|(_, total)| *total >= 2)
        .cloned()
        .collect();
    assert!(
        !multi.is_empty(),
        "case setup: the first page of a default scan must reach a product offering 2+ \
         fulfillment options, got {reachable:?}"
    );

    // Rows: every multi-option product the scan reaches, and for each of them every page
    // size from one option up to the whole list and one past it, so the truncation is
    // checked where it cuts, where it exactly fits and where it has nothing to cut.
    for (product_id, total) in multi {
        let all = all_options(&router, &product_id).await;
        for page_size in 1..=total + 1 {
            let mut seen: Vec<Value> = Vec::new();
            let mut next_token: Option<String> = None;
            let mut pages = 0usize;
            loop {
                let mut input = json!({ "productId": product_id, "maxResults": page_size });
                if let Some(token) = &next_token {
                    input["nextToken"] = json!(token);
                }
                let body = ok(&router, "listFulfillmentOptions", &input).await;
                let page = array(&body, "fulfillmentOptions");
                let expected = page_size.min(total - seen.len());
                assert_eq!(
                    page.len(),
                    expected,
                    "case {product_id} maxResults={page_size} page {pages}: expected {expected} \
                     of {total} options, got {}",
                    page.len()
                );
                seen.extend(page);
                pages += 1;
                next_token = body
                    .get("nextToken")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let more = seen.len() < total;
                assert_eq!(
                    next_token.is_some(),
                    more,
                    "case {product_id} maxResults={page_size} page {pages}: a nextToken is \
                     returned exactly when options remain ({} of {total} seen), got {body}",
                    seen.len()
                );
                if next_token.is_none() {
                    break;
                }
                assert!(
                    pages <= total,
                    "case {product_id} maxResults={page_size}: paging must terminate, {pages} \
                     pages for {total} options"
                );
            }
            assert_eq!(
                seen, all,
                "case {product_id} maxResults={page_size}: paging must yield every option once, \
                 in the order one whole page reports them"
            );
            let expected_pages = total.div_ceil(page_size);
            assert_eq!(
                pages, expected_pages,
                "case {product_id} maxResults={page_size}: expected {expected_pages} pages, got \
                 {pages}"
            );
        }
    }
}

#[tokio::test]
async fn a_reached_products_options_are_the_routes_its_listing_summarises() {
    let router = router();

    // A product's option list and the `fulfillmentOptionSummaries` of the listing it was
    // reached through are two readings of one record, so a product offered through several
    // routes is summarised as offering all of them.
    let page = ok(&router, "searchListings", &json!({ "maxResults": 20 })).await;
    let mut multi_route_listings = 0usize;
    // Rows: every listing of the scan, which differ in how many routes their products are
    // delivered through.
    for summary in array(&page, "listingSummaries") {
        let listing_id = text(&summary, "listingId");
        let mut types: Vec<String> = Vec::new();
        let mut options = 0usize;
        for entity in array(&summary, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            for option in all_options(&router, &text(product, "productId")).await {
                options += 1;
                let detail = option
                    .as_object()
                    .and_then(|members| members.values().next().cloned())
                    .unwrap_or(Value::Null);
                let option_type = text(&detail, "fulfillmentOptionType");
                if !types.contains(&option_type) {
                    types.push(option_type);
                }
            }
        }
        if options >= 2 {
            multi_route_listings += 1;
        }
        types.sort();
        let mut summarised: Vec<String> = array(&summary, "fulfillmentOptionSummaries")
            .iter()
            .map(|entry| text(entry, "fulfillmentOptionType"))
            .collect();
        summarised.sort();
        assert_eq!(
            summarised, types,
            "case {listing_id}: the summary must name every fulfillment option type the \
             listing's products offer, got {summarised:?} for {types:?}"
        );
    }
    assert!(
        multi_route_listings > 0,
        "case maxResults=20: a scan must reach a listing whose products offer 2+ fulfillment \
         options"
    );
}
