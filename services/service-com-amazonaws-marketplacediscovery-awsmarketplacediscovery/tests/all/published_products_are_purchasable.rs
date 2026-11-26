// SPDX-License-Identifier: CC0-1.0

//! What the catalog PUBLISHES and what it SELLS are one catalog: every product a discovery
//! read reports resolves, under a `PRODUCT_ID` filter, to the purchase options that sell it,
//! and an offer set reached that way resolves to the purchase options it bundles.
//!
//! The rules asserted here, each over the WHOLE seeded catalog rather than one sampled
//! record:
//!
//! 1. every product `SearchListings` publishes (and `GetProduct` reads back) is named by at
//!    least one `OFFER` purchase option, and every row a `PRODUCT_ID` filter returns covers
//!    the filtered product in its `associatedEntities`
//!    (`smithy_query(query="shape",shape="PurchaseOptionSummary,PurchaseOptionAssociatedEntity")`);
//! 2. ANDing a `SELLER_OF_RECORD_PROFILE_ID` filter for the seller of record of those rows
//!    selects exactly the rows that seller sells — "Multiple filters are combined with AND
//!    logic. Multiple values within the same filter are combined with OR logic"
//!    (`ListPurchaseOptionsInput$filters`);
//! 3. an offer set those rows name resolves, through `GetOfferSet` and through each product
//!    it bundles, back to its own `OFFERSET` row and to the offers it bundles;
//! 4. a `PRODUCT_ID` value no record of the catalog carries selects nothing, and that is an
//!    empty page rather than an error: `ListPurchaseOptions` models `AccessDeniedException`,
//!    `ValidationException`, `ThrottlingException` and `InternalServerException` and no
//!    "nothing matched" error at all
//!    (`smithy_query(query="operation",operation="ListPurchaseOptions")`).
//!
//! Every suite drives real HTTP requests through the `MultiServiceRouter`, so the protocol,
//! the routing table, input validation and the handlers are all exercised.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::catalog_drilldown::{array, ok, router, text, whole_catalog};

/// Every `purchaseOptions` row a `filters` list selects, paged to exhaustion.
async fn purchase_options(router: &MultiServiceRouter, filters: &Value) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut input = json!({ "filters": filters, "maxResults": 100 });
        if let Some(token) = &token {
            input["nextToken"] = json!(token);
        }
        let body = ok(router, "listPurchaseOptions", &input).await;
        rows.extend(array(&body, "purchaseOptions"));
        match body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
        assert!(
            rows.len() < 10_000,
            "case filters={filters}: paging must terminate, collected {} rows",
            rows.len()
        );
    }
    rows
}

/// The `PRODUCT_ID` filter for one product.
fn product_filter(product_id: &str) -> Value {
    json!([{ "filterType": "PRODUCT_ID", "filterValues": [product_id] }])
}

/// The ids one member of each `associatedEntities` entry carries, in row order.
fn associated(row: &Value, member: &str, id_member: &str) -> Vec<String> {
    array(row, "associatedEntities")
        .iter()
        .filter_map(|entity| entity.get(member).cloned())
        .map(|entity| text(&entity, id_member))
        .collect()
}

/// The seller profile id of a purchase option's seller of record.
fn seller_of(row: &Value) -> String {
    row.get("sellerOfRecord")
        .map(|seller| text(seller, "sellerProfileId"))
        .unwrap_or_default()
}

fn option_ids(rows: &[Value]) -> Vec<String> {
    rows.iter().map(|row| text(row, "purchaseOptionId")).collect()
}

/// Rules 1 and 2, for every product the catalog publishes.
#[tokio::test]
async fn every_published_product_resolves_to_the_purchase_options_that_sell_it() {
    let router = router();
    let listings = whole_catalog(&router).await;

    // The rows of this table are the catalog's own published products: they differ in the
    // seller that publishes them, in how many products their listing associates, in how
    // many purchase options sell them, and in whether an offer set bundles them.
    let mut types: BTreeSet<String> = BTreeSet::new();
    let mut sellers: BTreeSet<String> = BTreeSet::new();
    let mut offer_sets: BTreeSet<String> = BTreeSet::new();
    let mut multi_product_listings = 0_usize;
    let mut products_seen = 0_usize;

    for listing in &listings {
        let listing_id = text(listing, "listingId");
        let product_ids: Vec<String> = associated(listing, "product", "productId");
        assert!(
            !product_ids.is_empty(),
            "case listing {listing_id}: a published listing names the products it \
             associates, got {listing}"
        );
        if product_ids.len() > 1 {
            multi_product_listings += 1;
        }

        for product_id in &product_ids {
            products_seen += 1;
            let case = format!("product {product_id} of listing {listing_id}");

            // The detail read publishes the same product, so the id the filter is given is
            // the id this catalog reports rather than one the test made up.
            let product = ok(
                &router,
                "getProduct",
                &json!({ "productId": product_id.clone() }),
            )
            .await;
            assert_eq!(
                text(&product, "productId"),
                *product_id,
                "case {case}: getProduct must report the product the listing names, got \
                 {product}"
            );

            let rows = purchase_options(&router, &product_filter(product_id)).await;
            assert!(
                !rows.is_empty(),
                "case {case}: a product the catalog publishes must resolve to the purchase \
                 options that sell it, got an empty page"
            );

            let mut offer_rows = 0_usize;
            for row in &rows {
                let option_id = text(row, "purchaseOptionId");
                let option_type = text(row, "purchaseOptionType");
                let row_case = format!("{case}, purchase option {option_id}");
                assert!(
                    !option_id.is_empty(),
                    "case {row_case}: purchaseOptionId is required, got {row}"
                );
                assert!(
                    option_type == "OFFER" || option_type == "OFFERSET",
                    "case {row_case}: purchaseOptionType must be OFFER or OFFERSET, got \
                     {option_type:?}"
                );
                if option_type == "OFFER" {
                    offer_rows += 1;
                }
                types.insert(option_type.clone());
                assert_eq!(
                    text(row, "catalog"),
                    text(&product, "catalog"),
                    "case {row_case}: the row's catalog must be the product's, got {row}"
                );
                let row_seller = seller_of(row);
                assert!(
                    !row_seller.is_empty(),
                    "case {row_case}: sellerOfRecord is required, got {row}"
                );
                sellers.insert(row_seller);
                let covered = associated(row, "product", "productId");
                assert!(
                    covered.contains(product_id),
                    "case {row_case}: the row must cover the filtered product, got {covered:?}"
                );
                for entity in array(row, "associatedEntities") {
                    assert!(
                        entity.get("product").is_some() && entity.get("offer").is_some(),
                        "case {row_case}: each associated entity pairs a product with the \
                         offer that sells it, got {entity}"
                    );
                }
            }
            assert!(
                offer_rows > 0,
                "case {case}: a published product must be sold by at least one OFFER \
                 purchase option, got {:?}",
                rows.iter()
                    .map(|row| text(row, "purchaseOptionType"))
                    .collect::<Vec<_>>()
            );

            // Rule 2: ANDing the seller of record of the rows narrows to exactly the rows
            // that seller sells, computed from the unfiltered page rather than written out.
            let row_sellers: BTreeSet<String> = rows.iter().map(seller_of).collect();
            for seller in &row_sellers {
                let expected: Vec<String> = option_ids(
                    &rows
                        .iter()
                        .filter(|row| &seller_of(row) == seller)
                        .cloned()
                        .collect::<Vec<Value>>(),
                );
                let narrowed = purchase_options(
                    &router,
                    &json!([
                        { "filterType": "PRODUCT_ID", "filterValues": [product_id] },
                        { "filterType": "SELLER_OF_RECORD_PROFILE_ID", "filterValues": [seller] },
                    ]),
                )
                .await;
                let actual = option_ids(&narrowed);
                assert_eq!(
                    actual, expected,
                    "case {case} AND seller {seller}: expected {expected:?}, got {actual:?}"
                );
            }

            // Rule 3's premise: an offer set an OFFER row names is itself one of the
            // product's purchase options, so the drill-down into the set has a row to reach.
            for row in &rows {
                if text(row, "purchaseOptionType") != "OFFER" {
                    continue;
                }
                for entity in array(row, "associatedEntities") {
                    let Some(offer_set) = entity.get("offerSet") else {
                        continue;
                    };
                    let offer_set_id = text(offer_set, "offerSetId");
                    offer_sets.insert(offer_set_id.clone());
                    assert!(
                        rows.iter().any(|candidate| {
                            text(candidate, "purchaseOptionType") == "OFFERSET"
                                && (text(candidate, "purchaseOptionId") == offer_set_id
                                    || associated(candidate, "offerSet", "offerSetId")
                                        .contains(&offer_set_id))
                        }),
                        "case {case}: the offer set {offer_set_id} that bundles this \
                         product's offer must itself be one of its purchase options, got \
                         {:?}",
                        option_ids(&rows)
                    );
                }
            }
        }
    }

    // The table spans the class: both purchase option types, several sellers, a listing
    // with more than one product, and products an offer set bundles.
    assert!(
        products_seen > 10,
        "case whole catalog: the catalog must publish many products, got {products_seen}"
    );
    assert_eq!(
        types,
        ["OFFER", "OFFERSET"]
            .iter()
            .map(|kind| (*kind).to_string())
            .collect::<BTreeSet<String>>(),
        "case whole catalog: both purchase option types must be reachable from a published \
         product, got {types:?}"
    );
    assert!(
        sellers.len() > 1,
        "case whole catalog: the catalog's purchase options must come from several sellers \
         of record, got {sellers:?}"
    );
    assert!(
        multi_product_listings > 0,
        "case whole catalog: at least one listing must associate several products"
    );
    assert!(
        !offer_sets.is_empty(),
        "case whole catalog: at least one published product must be bundled by an offer set"
    );
}

/// Rule 3: an offer set the catalog publishes resolves to the purchase options it bundles.
#[tokio::test]
async fn an_offer_set_resolves_to_the_purchase_options_it_bundles() {
    let router = router();

    // Every offer set the catalog sells, reached the way a client reaches one: through the
    // purchase options of the products it bundles.
    let mut offer_set_ids: BTreeSet<String> = BTreeSet::new();
    for listing in &whole_catalog(&router).await {
        for product_id in associated(listing, "product", "productId") {
            for row in purchase_options(&router, &product_filter(&product_id)).await {
                if text(&row, "purchaseOptionType") == "OFFERSET" {
                    offer_set_ids.insert(text(&row, "purchaseOptionId"));
                }
                for entity in array(&row, "associatedEntities") {
                    if let Some(offer_set) = entity.get("offerSet") {
                        offer_set_ids.insert(text(offer_set, "offerSetId"));
                    }
                }
            }
        }
    }
    assert!(
        !offer_set_ids.is_empty(),
        "case whole catalog: the catalog must sell at least one offer set"
    );

    for offer_set_id in &offer_set_ids {
        let detail = ok(
            &router,
            "getOfferSet",
            &json!({ "offerSetId": offer_set_id.clone() }),
        )
        .await;
        assert_eq!(
            text(&detail, "offerSetId"),
            *offer_set_id,
            "case offer set {offer_set_id}: getOfferSet must read back the set, got {detail}"
        );
        let bundled: Vec<(String, String)> = array(&detail, "associatedEntities")
            .iter()
            .filter_map(|entity| {
                let product = entity.get("product")?;
                let offer = entity.get("offer")?;
                Some((text(product, "productId"), text(offer, "offerId")))
            })
            .collect();
        assert!(
            !bundled.is_empty(),
            "case offer set {offer_set_id}: an offer set bundles the products and offers it \
             sells, got {detail}"
        );

        for (product_id, offer_id) in &bundled {
            let case = format!("offer set {offer_set_id}, product {product_id}");
            let rows = purchase_options(&router, &product_filter(product_id)).await;
            assert!(
                rows.iter().any(|row| {
                    text(row, "purchaseOptionType") == "OFFERSET"
                        && (text(row, "purchaseOptionId") == *offer_set_id
                            || associated(row, "offerSet", "offerSetId").contains(offer_set_id))
                }),
                "case {case}: the offer set must be among the product's purchase options, \
                 got {:?}",
                option_ids(&rows)
            );
            assert!(
                rows.iter().any(|row| {
                    text(row, "purchaseOptionType") == "OFFER"
                        && associated(row, "offer", "offerId").contains(offer_id)
                }),
                "case {case}: the offer {offer_id} the set bundles must be among the \
                 product's purchase options, got {:?}",
                option_ids(&rows)
            );
        }
    }
}

/// Rule 4: an identifier no record of the catalog carries selects nothing, and that is an
/// empty page rather than an error — for every FORM such an identifier takes.
#[tokio::test]
async fn an_identifier_the_catalog_does_not_carry_selects_an_empty_page() {
    let router = router();

    // A product the catalog does publish, as the control: the empty pages below are about
    // the identifier, not about the operation.
    let listings = whole_catalog(&router).await;
    let (known_product, known_seller) = {
        let mut found: Option<(String, String)> = None;
        for listing in &listings {
            for product_id in associated(listing, "product", "productId") {
                let rows = purchase_options(&router, &product_filter(&product_id)).await;
                if let Some(row) = rows.first() {
                    found = Some((product_id, seller_of(row)));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        found.expect("a published product with a purchase option")
    };
    let control = purchase_options(&router, &product_filter(&known_product)).await;
    assert!(
        !control.is_empty(),
        "case control PRODUCT_ID={known_product}: a published product must resolve to its \
         purchase options"
    );

    // Every value below satisfies `PurchaseOptionFilterValue` — @length 1..255, @pattern
    // ^[\w\-]+$ (`smithy_query(query="shape",shape="PurchaseOptionFilterValue")`) — so the
    // request is well formed and reaches the catalog: the forms differ in shape (this
    // catalog's own id shape, a 36-character hyphenated identifier, a word-character
    // identifier) and in length (the modelled minimum and maximum).
    let maximum_length = "a".repeat(255);
    let unknown_products: [&str; 5] = [
        "p-9999-no-such-product",
        "11111111-2222-4333-8444-555555555555",
        "Product_0001",
        "x",
        maximum_length.as_str(),
    ];
    for unknown in unknown_products {
        let body = ok(
            &router,
            "listPurchaseOptions",
            &json!({
                "filters": product_filter(unknown),
                "maxResults": 100,
            }),
        )
        .await;
        let rows = array(&body, "purchaseOptions");
        assert!(
            rows.is_empty(),
            "case PRODUCT_ID={unknown}: an identifier the catalog does not carry selects \
             nothing, got {:?}",
            option_ids(&rows)
        );
        assert!(
            body.get("nextToken").is_none(),
            "case PRODUCT_ID={unknown}: an empty page has no further page, got {body}"
        );
    }

    // AND across filters: a pair in which either half names nothing selects nothing, even
    // when the other half names a record the catalog holds.
    let pairs: [(&str, &str, &str); 3] = [
        (
            "unknown product, known seller",
            "p-9999-no-such-product",
            known_seller.as_str(),
        ),
        (
            "known product, unknown seller",
            known_product.as_str(),
            "sp-9999-no-such-seller",
        ),
        (
            "both unknown",
            "11111111-2222-4333-8444-555555555555",
            "99999999-8888-4777-8666-555555555555",
        ),
    ];
    for (case, product, seller) in pairs {
        let rows = purchase_options(
            &router,
            &json!([
                { "filterType": "PRODUCT_ID", "filterValues": [product] },
                { "filterType": "SELLER_OF_RECORD_PROFILE_ID", "filterValues": [seller] },
            ]),
        )
        .await;
        assert!(
            rows.is_empty(),
            "case {case} (PRODUCT_ID={product}, SELLER_OF_RECORD_PROFILE_ID={seller}): \
             expected an empty page, got {:?}",
            option_ids(&rows)
        );
    }

    // OR within one filter: the known product ORed with one the catalog does not carry
    // selects exactly the known product's own rows.
    let ored = purchase_options(
        &router,
        &json!([{
            "filterType": "PRODUCT_ID",
            "filterValues": [known_product.clone(), "p-9999-no-such-product"],
        }]),
    )
    .await;
    assert_eq!(
        option_ids(&ored),
        option_ids(&control),
        "case PRODUCT_ID=[{known_product}, p-9999-no-such-product]: OR within one filter \
         must select the known product's rows"
    );
}
