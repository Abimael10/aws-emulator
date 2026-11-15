// SPDX-License-Identifier: CC0-1.0

//! How MANY fulfillment options a catalog product offers, and what follows from that.
//!
//! THE RULE UNDER TEST: a product's fulfillment options are the ones ITS OWN RECORD carries
//! — `Product` carries a `FulfillmentOptionsList`, so a record offers one route or several,
//! and `ListFulfillmentOptions` is `@paginated` with a `maxResults` of its own so a client
//! can page the ones it offers. Two things follow, and each is asserted here:
//!
//! 1. the catalog holds products offering several options, so the truncation and
//!    continuation paths of this paginated operation are reachable at all;
//! 2. a page is bounded by `maxResults` — exactly `maxResults` items while items remain,
//!    with a `nextToken` for the remainder — and walking the tokens yields every option
//!    once, in the order one whole page reports them.
//!
//! That the options a product reports are exactly the ones its record offers — and that
//! nothing else the product reports contradicts them — is asserted in
//! `fulfillment_option_records`.
//!
//! Every request goes through the `MultiServiceRouter`, so the protocol, the routing table
//! and the handlers are all exercised end to end.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::catalog_drilldown::{array, call, ok, router, text};

/// How many listing summaries the scan below reads: the model's largest `SearchListings`
/// page, which the catalog fits (`catalog_record_breadth` asserts it does).
const SCANNED_LISTINGS: i32 = 100;

/// The `fulfillmentOptionType` of every option on one page, read from the union member each
/// option carries.
fn option_types(page: &Value) -> Vec<String> {
    array(page, "fulfillmentOptions")
        .iter()
        .filter_map(|option| {
            option
                .as_object()
                .and_then(|object| object.values().next().cloned())
        })
        .map(|member| text(&member, "fulfillmentOptionType"))
        .collect()
}

/// Every product id `SearchListings` reaches, through the listings it summarises.
async fn scanned_product_ids(router: &MultiServiceRouter) -> Vec<String> {
    let page = ok(
        router,
        "searchListings",
        &json!({ "maxResults": SCANNED_LISTINGS }),
    )
    .await;
    let mut product_ids: Vec<String> = Vec::new();
    for summary in array(&page, "listingSummaries") {
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
        "case setup: the scan must reach a product, got {page}"
    );
    product_ids
}

/// The ids of the products the catalog reports more than one fulfillment option for.
async fn multi_option_product_ids(router: &MultiServiceRouter) -> Vec<String> {
    let mut multi: Vec<String> = Vec::new();
    for product_id in scanned_product_ids(router).await {
        // The model's largest page (50) is more than any product offers, so one page is all
        // of them.
        let page = ok(
            router,
            "listFulfillmentOptions",
            &json!({ "productId": product_id.clone(), "maxResults": 50 }),
        )
        .await;
        let types = option_types(&page);
        assert!(
            !types.is_empty(),
            "case {product_id}: a catalog product must report how it is fulfilled, got {page}"
        );
        let distinct: BTreeSet<&String> = types.iter().collect();
        assert_eq!(
            distinct.len(),
            types.len(),
            "case {product_id}: a product offers each delivery route once, got {types:?}"
        );
        if types.len() > 1 {
            multi.push(product_id);
        }
    }
    multi
}

/// RULE 1: the catalog holds products whose records offer several fulfillment options, so a
/// paginating client is exercised at all.
#[tokio::test]
async fn the_catalog_holds_products_with_several_fulfillment_options() {
    let router = router();
    let multi = multi_option_product_ids(&router).await;
    assert!(
        multi.len() > 1,
        "case the whole catalog: several products must offer more than one fulfillment \
         option, got {multi:?}"
    );
}

/// RULE 2: `maxResults` bounds a page and `nextToken` walks the rest, over a table of page
/// sizes against the products that offer several options.
#[tokio::test]
async fn max_results_truncates_a_page_and_next_token_walks_the_remainder() {
    let router = router();
    let product_ids = multi_option_product_ids(&router).await;
    assert!(
        !product_ids.is_empty(),
        "case setup: this rule needs a product with several options"
    );

    let mut exercised = 0_usize;
    for product_id in product_ids.iter().take(4) {
        let whole = ok(
            &router,
            "listFulfillmentOptions",
            &json!({ "productId": product_id, "maxResults": 50 }),
        )
        .await;
        let all = array(&whole, "fulfillmentOptions");
        assert!(
            whole.get("nextToken").is_none_or(Value::is_null),
            "case {product_id}: a page holding every option carries no nextToken, got {whole}"
        );
        let total = all.len();
        assert!(
            total > 1,
            "case {product_id}: this rule needs a product with several options, got {total}"
        );

        // Rows: the model's `maxResults` minimum, every size short of the whole set, the set
        // itself and one past it (`smithy_query(query="operation",
        // operation="ListFulfillmentOptions")` declares @range 1..50).
        for page_size in 1..=total + 1 {
            let mut seen: Vec<Value> = Vec::new();
            let mut next_token: Option<String> = None;
            let mut pages = 0_usize;
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
                     options, got {}",
                    page.len()
                );
                seen.extend(page);
                pages += 1;
                next_token = body
                    .get("nextToken")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                // A token is returned exactly while options remain.
                assert_eq!(
                    next_token.is_some(),
                    seen.len() < total,
                    "case {product_id} maxResults={page_size} page {pages}: a nextToken must be \
                     returned if and only if options remain, got {body}"
                );
                if next_token.is_none() {
                    break;
                }
            }
            assert_eq!(
                seen, all,
                "case {product_id} maxResults={page_size}: paging must yield every option once, \
                 in the order one whole page reports them"
            );
            assert_eq!(
                pages,
                total.div_ceil(page_size),
                "case {product_id} maxResults={page_size}: expected {} pages, got {pages}",
                total.div_ceil(page_size)
            );
            exercised += 1;
        }
    }
    assert!(
        exercised > 4,
        "case the scanned products: expected several page sizes per product, exercised only \
         {exercised}"
    );
}

/// A `maxResults` outside the modelled range is still refused, so the truncation above is
/// the operation honouring a bound rather than ignoring one.
#[tokio::test]
async fn a_max_results_outside_the_modelled_range_is_refused() {
    let router = router();
    let product_id = scanned_product_ids(&router).await[0].clone();

    // `maxResults` is @range 1..50, so each published bound is accepted and the value one
    // past it is refused.
    for (max_results, expected) in [(1, 200), (50, 200), (0, 400), (51, 400)] {
        let (status, body) = call(
            &router,
            "listFulfillmentOptions",
            &json!({ "productId": product_id, "maxResults": max_results }),
        )
        .await;
        assert_eq!(
            status, expected,
            "case maxResults={max_results}: expected HTTP {expected}, got {status} with {body}"
        );
    }
}
