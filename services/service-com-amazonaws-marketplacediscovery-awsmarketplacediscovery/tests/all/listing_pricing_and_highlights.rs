// SPDX-License-Identifier: CC0-1.0

//! What a listing and its product report for the pricing they are sold under and the key
//! features they offer, over the WHOLE seeded catalog.
//!
//! The rules under test:
//!
//! * a listing's `pricingModels` are "[t]he pricing models for offers associated with this
//!   listing" and its `pricingUnits` "[t]he pricing units that define the billing dimensions
//!   for offers associated with this listing"
//!   (`smithy_query(query="shape",shape="GetListingOutput")`), so both are read off the
//!   listing's OWN offers — a listing can neither advertise pricing no offer of it carries
//!   nor omit pricing an offer of it does;
//! * one pricing model or unit type is displayed by one name across the whole catalog
//!   (`PricingModel$displayName`, `PricingUnit$displayName`);
//! * `highlights` is "[a] list of key features that the listing offers to customers", so a
//!   record's highlights are features of the record and not a restatement of how it is
//!   classified, delivered or priced — and a product states the features of its own record
//!   rather than a shortened copy of its listing's;
//! * a listing's badges state attributes its own records have, so a listing whose records
//!   have none reports an EMPTY list, and `AWS_FREE_TIER` appears only for a listing an
//!   offer of which is priced `FREE`.
//!
//! Every suite drives real HTTP requests through the `MultiServiceRouter`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::ops::types::{
    PRICING_MODEL_TYPE_VALUES, PRICING_UNIT_TYPE_VALUES,
};

use crate::catalog_drilldown::{array, ok, router, text};

/// Every listing summary the catalog holds, one page over the whole of it.
async fn whole_catalog(router: &MultiServiceRouter) -> Vec<Value> {
    let mut summaries: Vec<Value> = Vec::new();
    let mut token = String::new();
    loop {
        let mut input = json!({ "maxResults": 100 });
        if !token.is_empty() {
            input["nextToken"] = Value::String(token.clone());
        }
        let page = ok(router, "searchListings", &input).await;
        summaries.extend(array(&page, "listingSummaries"));
        token = text(&page, "nextToken");
        if token.is_empty() {
            break;
        }
    }
    assert!(
        summaries.len() > 25,
        "case setup: the catalog must hold more listings than one default page, got {}",
        summaries.len()
    );
    summaries
}

/// The offer ids a listing associates, read off its own `associatedEntities`.
fn associated_offer_ids(listing: &Value) -> Vec<String> {
    array(listing, "associatedEntities")
        .iter()
        .filter_map(|entity| entity.get("offer").cloned())
        .map(|offer| text(&offer, "offerId"))
        .collect()
}

/// The `(type, displayName)` pairs a list member of a response carries.
fn typed_names(body: &Value, member: &str, type_key: &str) -> Vec<(String, String)> {
    array(body, member)
        .iter()
        .map(|item| (text(item, type_key), text(item, "displayName")))
        .collect()
}

/// Whether `phrase` appears in `sentence` as a contiguous run of whole words, compared
/// case-insensitively — the test for "this highlight is a restatement of that display name"
/// that does not trip over a name that happens to sit inside a longer word.
fn states_phrase(sentence: &str, phrase: &str) -> bool {
    let words = |value: &str| -> Vec<String> {
        value
            .split(|character: char| !character.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let sentence = words(sentence);
    let phrase = words(phrase);
    if phrase.is_empty() || phrase.len() > sentence.len() {
        return false;
    }
    sentence
        .windows(phrase.len())
        .any(|window| window == phrase.as_slice())
}

/// A listing's pricing models and units are exactly those of the offers it associates, and
/// the detail read and the search summary agree on them.
#[tokio::test]
async fn a_listings_pricing_is_the_pricing_of_the_offers_it_associates() {
    let router = router();

    // Rows: every listing in the catalog — they differ in how many offers they associate
    // (one, two), in pricing model, in billing dimension and in which seeding tier wrote
    // them, so the expectation is computed from each listing's own offers rather than
    // written by hand.
    let mut models_seen: BTreeSet<String> = BTreeSet::new();
    let mut units_seen: BTreeSet<String> = BTreeSet::new();
    let mut display_names: BTreeMap<String, String> = BTreeMap::new();
    for summary in whole_catalog(&router).await {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;

        // What the listing's OWN offers are priced under.
        let mut offer_models: BTreeSet<String> = BTreeSet::new();
        for offer_id in associated_offer_ids(&listing) {
            let offer = ok(&router, "getOffer", &json!({ "offerId": offer_id.clone() })).await;
            let model = offer
                .get("pricingModel")
                .map(|model| text(model, "pricingModelType"))
                .unwrap_or_default();
            assert!(
                !model.is_empty(),
                "case getOffer {offer_id}: an offer states the pricing model it is sold \
                 under, got {offer}"
            );
            offer_models.insert(model);
        }

        let reported = typed_names(&listing, "pricingModels", "pricingModelType");
        let reported_models: BTreeSet<String> =
            reported.iter().map(|(value, _)| value.clone()).collect();
        assert_eq!(
            reported_models, offer_models,
            "case getListing {listing_id}: expected the pricing models of its own offers \
             {offer_models:?}, got {reported_models:?}"
        );
        assert_eq!(
            reported.len(),
            reported_models.len(),
            "case getListing {listing_id}: a pricing model is reported once, got {reported:?}"
        );

        let units = typed_names(&listing, "pricingUnits", "pricingUnitType");
        let unit_types: BTreeSet<String> = units.iter().map(|(value, _)| value.clone()).collect();
        assert_eq!(
            units.len(),
            unit_types.len(),
            "case getListing {listing_id}: a pricing unit is reported once, got {units:?}"
        );

        // Every value is a modelled enum value displayed by a non-blank name, and one type
        // is displayed by ONE name across the whole catalog.
        for (member, values, pairs) in [
            ("pricingModels", PRICING_MODEL_TYPE_VALUES, &reported),
            ("pricingUnits", PRICING_UNIT_TYPE_VALUES, &units),
        ] {
            for (value, display_name) in pairs {
                assert!(
                    values.contains(&value.as_str()),
                    "case getListing {listing_id} {member}: expected one of {values:?}, got \
                     {value:?}"
                );
                assert!(
                    !display_name.trim().is_empty(),
                    "case getListing {listing_id} {member}={value}: displayName is required \
                     and non-blank, got {display_name:?}"
                );
                if let Some(held) = display_names.get(value) {
                    assert_eq!(
                        held, display_name,
                        "case getListing {listing_id} {member}={value}: the catalog displays \
                         that type by {held:?}, got {display_name:?}"
                    );
                } else {
                    display_names.insert(value.clone(), display_name.clone());
                }
            }
        }

        // The search summary and the detail read answer from the same projection.
        for member in ["pricingModels", "pricingUnits"] {
            assert_eq!(
                summary.get(member),
                listing.get(member),
                "case {listing_id} {member}: searchListings and getListing must report the \
                 same pricing, got {:?} and {:?}",
                summary.get(member),
                listing.get(member)
            );
        }

        models_seen.extend(reported_models);
        units_seen.extend(unit_types);
    }

    // The catalog exercises every pricing model and every billing dimension the model
    // declares, so none of the rules above is asserted only in the negative.
    let expected_models: BTreeSet<String> = PRICING_MODEL_TYPE_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    let expected_units: BTreeSet<String> = PRICING_UNIT_TYPE_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    assert_eq!(
        models_seen, expected_models,
        "case whole catalog: every modelled pricing model must be reachable, got \
         {models_seen:?}"
    );
    assert_eq!(
        units_seen, expected_units,
        "case whole catalog: every modelled pricing unit must be reachable, got {units_seen:?}"
    );
}

/// A usage-priced listing's billing dimensions are the ones its offers' rate cards meter:
/// the pricing unit a listing reports is backed by a term of an offer it associates.
#[tokio::test]
async fn a_metered_listings_pricing_units_are_the_dimensions_its_offer_terms_meter() {
    let router = router();

    // Rows: every usage-priced listing in the catalog, whose offers carry a rate card.
    let mut checked = 0_usize;
    for summary in whole_catalog(&router).await {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        let usage_priced = array(&listing, "pricingModels")
            .iter()
            .any(|model| text(model, "pricingModelType") == "USAGE");
        if !usage_priced {
            continue;
        }

        // The units the listing's own offer terms meter, read off their rate cards.
        let mut metered: BTreeSet<String> = BTreeSet::new();
        for offer_id in associated_offer_ids(&listing) {
            let terms = ok(
                &router,
                "getOfferTerms",
                &json!({ "offerId": offer_id, "maxResults": 25 }),
            )
            .await;
            for term in array(&terms, "offerTerms") {
                let Some(usage) = term.get("usageBasedPricingTerm") else {
                    continue;
                };
                for card in array(usage, "rateCards") {
                    for rate in array(&card, "rateCard") {
                        metered.insert(text(&rate, "unit"));
                    }
                }
            }
        }
        if metered.is_empty() {
            continue;
        }

        for (unit_type, display_name) in typed_names(&listing, "pricingUnits", "pricingUnitType") {
            assert!(
                metered.contains(&display_name),
                "case getListing {listing_id}: pricing unit {unit_type} ({display_name:?}) must \
                 be a dimension an offer of this listing meters, got {metered:?}"
            );
        }
        checked += 1;
    }
    assert!(
        checked > 2,
        "case setup: the catalog must hold several metered listings, got {checked}"
    );
}

/// A record's highlights are its own key features: a feature sentence, not a restatement of
/// the record's categories, fulfillment or pricing — and a single-product listing and its
/// product report the same ones.
#[tokio::test]
async fn highlights_are_the_records_own_key_features() {
    let router = router();

    // Rows: every listing in the catalog and every product it associates — single-product
    // and multi-product listings, single-route and multi-route products, both seeding tiers
    // and the hand-written records.
    let mut single_product_listings = 0_usize;
    for summary in whole_catalog(&router).await {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        let products: Vec<String> = array(&listing, "associatedEntities")
            .iter()
            .filter_map(|entity| entity.get("product").cloned())
            .map(|product| text(&product, "productId"))
            .collect();

        let mut records = vec![(format!("getListing {listing_id}"), listing.clone())];
        for product_id in &products {
            let product = ok(
                &router,
                "getProduct",
                &json!({ "productId": product_id.clone() }),
            )
            .await;
            records.push((format!("getProduct {product_id}"), product));
        }

        for (case, record) in &records {
            let highlights: Vec<String> = array(record, "highlights")
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect();
            assert!(
                !highlights.is_empty() && highlights.len() <= 10,
                "case {case}: a record states between one and ten key features \
                 (HighlightList @length ..10), got {highlights:?}"
            );
            // What the record ALREADY reports elsewhere: a key feature restates none of it.
            let mut classification: Vec<String> = Vec::new();
            classification.extend(
                array(record, "categories")
                    .iter()
                    .map(|category| text(category, "displayName")),
            );
            classification.extend(
                array(record, "fulfillmentOptionSummaries")
                    .iter()
                    .map(|option| text(option, "displayName")),
            );
            classification.extend(
                array(record, "pricingModels")
                    .iter()
                    .map(|model| text(model, "displayName")),
            );
            classification.extend(
                array(record, "pricingUnits")
                    .iter()
                    .map(|unit| text(unit, "displayName")),
            );
            for highlight in &highlights {
                assert!(
                    !highlight.trim().is_empty(),
                    "case {case}: a highlight is non-blank, got {highlight:?}"
                );
                for stated in &classification {
                    assert!(
                        !states_phrase(highlight, stated),
                        "case {case}: the key feature {highlight:?} must not restate the \
                         record's own {stated:?}"
                    );
                }
            }
        }

        // A listing with ONE product is one record seen two ways: the product states the
        // features of that record, never a shortened copy of the listing's list.
        if products.len() == 1 {
            single_product_listings += 1;
            let (case, product) = &records[1];
            let listing_features: BTreeSet<String> = array(&listing, "highlights")
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect();
            let product_features: BTreeSet<String> = array(product, "highlights")
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect();
            assert!(
                !(product_features.is_subset(&listing_features)
                    && product_features != listing_features),
                "case {case}: a product's key features are its own record's, not a shortened \
                 copy of its listing's {listing_features:?}, got {product_features:?}"
            );
        }
    }
    assert!(
        single_product_listings > 2,
        "case setup: the catalog must hold several single-product listings, got \
         {single_product_listings}"
    );
}

/// A listing's badges are backed by its own records: `AWS_FREE_TIER` only where an offer it
/// associates is priced `FREE`, and no badge at all for a listing whose records have no
/// special attribute.
#[tokio::test]
async fn a_free_tier_badge_is_backed_by_a_free_offer() {
    let router = router();

    // Rows: every listing in the catalog — those with a free offer, those without, and
    // those with no badge-bearing attribute at all.
    let mut free_tier_listings = 0_usize;
    let mut unbadged_listings = 0_usize;
    for summary in whole_catalog(&router).await {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        let badges: BTreeSet<String> = array(&listing, "badges")
            .iter()
            .map(|badge| text(badge, "badgeType"))
            .collect();
        let free_priced = array(&listing, "pricingModels")
            .iter()
            .any(|model| text(model, "pricingModelType") == "FREE");
        assert_eq!(
            badges.contains("AWS_FREE_TIER"),
            free_priced,
            "case getListing {listing_id}: an AWS_FREE_TIER badge is reported exactly when an \
             offer of the listing is priced FREE (free offer: {free_priced}), got {badges:?}"
        );
        if free_priced {
            free_tier_listings += 1;
        }
        if badges.is_empty() {
            unbadged_listings += 1;
        }
    }
    assert!(
        free_tier_listings > 0,
        "case setup: the catalog must hold a listing with a free offer, got \
         {free_tier_listings}"
    );
    assert!(
        unbadged_listings > 0,
        "case setup: the catalog must hold a listing that has earned no badge, so an empty \
         badge list is exercised, got {unbadged_listings}"
    );
}
