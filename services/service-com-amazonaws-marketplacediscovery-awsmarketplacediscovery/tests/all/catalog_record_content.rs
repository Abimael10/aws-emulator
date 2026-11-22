// SPDX-License-Identifier: CC0-1.0

//! What a catalog record's PROSE says, and where it comes from.
//!
//! The rule under test: the content members of a record — `longDescription` ("A detailed
//! description of what the listing offers, in paragraph format"), `highlights` ("A list of
//! key features that the listing offers to customers") and `useCases` ("Use cases associated
//! with the listing, describing scenarios where the product can be applied"), per
//! `smithy_query(query="shape",shape="GetListingOutput")` — are content OF the record, so a
//! detail read restates them as the catalog holds them and NONE of them is a restatement of
//! another member of the same response. The categories a record is classified under and the
//! routes it is fulfilled through are reported by `categories` and
//! `fulfillmentOptionSummaries`; a record's prose therefore never consists of one of those
//! values, or of a sentence formed from one, and the detailed description is prose of its own
//! rather than the summary with a tail appended.
//!
//! What follows from it, and is asserted here over EVERY listing and product the catalog
//! holds — the hand-written records, the breadth tier and the per-category tier alike:
//!
//! * every record states at least one key feature, within the modelled ten, each inside
//!   `HighlightList`'s member bounds and stated once;
//! * no key feature, and no use-case description, is a category display name, a fulfillment
//!   option display name, or a sentence formed from either;
//! * a promotional element is captioned by what it shows rather than by another member of
//!   the record repeated back;
//! * the detailed description is neither the short description nor the short description with
//!   more text appended;
//! * a listing and the product it associates state the SAME key features where the catalog
//!   holds one record for both, so the two detail views cannot disagree about what the
//!   product offers;
//! * reading a record twice returns the same content, because the content is held rather than
//!   composed per call.
//!
//! `SaasFulfillmentOption$quickLaunch`, the optional members of a fulfillment option and the
//! `QUICK_LAUNCH` badge derived from them are asserted in `fulfillment_option_records`; the
//! identifier and ordering of a record's categories in `catalog_record_breadth`.
//!
//! Every request goes through the `MultiServiceRouter`, so the protocol, the routing table
//! and the handlers are all exercised end to end.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::catalog_drilldown::{array, ok, products_of, router, text, whole_catalog};

/// `HighlightList` is `@length(max: 10)` and its member a `NonEmptyString`
/// `@length(1..1024)` (`smithy_query(query="shape",shape="HighlightList")`). A Smithy length
/// bound counts characters, so every check below counts characters rather than bytes.
const MAX_HIGHLIGHTS: usize = 10;
const MAX_HIGHLIGHT_LENGTH: usize = 1024;

/// `GetListingOutput$longDescription` and `GetProductOutput$longDescription` are
/// `NonEmptyString @length(max: 32768)`.
const MAX_LONG_DESCRIPTION_LENGTH: usize = 32768;

/// `UseCase`'s three members are each a `NonEmptyString @length(1..1024)`
/// (`smithy_query(query="shape",shape="UseCase")`).
const MAX_USE_CASE_LENGTH: usize = 1024;

/// The records whose listing and product views are compared against each other, as
/// `(slug of the record's ids, what this case varies)`.
///
/// Each row is a listing the catalog holds ONE record for, resolved by its slug so the case
/// does not depend on the sequence number the seeder gives it. They differ along what the
/// rule generalises over: how many routes the product is fulfilled through, how many
/// categories the record is classified under, and which kind of delivery it is — a record
/// whose prose was formerly composed from its single route is not the only shape of row.
const ONE_RECORD_ROWS: &[(&str, &str)] = &[
    ("aurora-linux-hardened", "two routes, one category"),
    ("borealis-market-data", "one route, two sibling leaf categories"),
    ("borealis-data-prep", "one route, two leaves of one subtree"),
    ("equinox-fleet-telemetry", "two routes, a hosted product"),
    ("cirrus-migration-studio", "one route, a services engagement"),
    ("cirrus-query-lens", "one route, a container product"),
];

/// The listing ids of the whole catalog, in the order a default search reports them.
async fn catalog_listing_ids(router: &MultiServiceRouter) -> Vec<String> {
    whole_catalog(router)
        .await
        .iter()
        .map(|summary| text(summary, "listingId"))
        .collect()
}

/// The listing whose id ends with `-{slug}`, as `GetListing` returns it.
async fn listing_of_slug(router: &MultiServiceRouter, slug: &str) -> Value {
    let suffix = format!("-{slug}");
    let listing_id = catalog_listing_ids(router)
        .await
        .into_iter()
        .find(|listing_id| listing_id.ends_with(&suffix))
        .unwrap_or_else(|| panic!("case {slug}: the catalog must hold a listing for this row"));
    ok(router, "getListing", &json!({ "listingId": listing_id })).await
}

/// The `displayName` of every element of a list member.
fn display_names(body: &Value, member: &str) -> Vec<String> {
    array(body, member)
        .iter()
        .map(|element| text(element, "displayName"))
        .collect()
}

/// The strings a record's prose must not consist of: a value another member of the SAME
/// response reports, and a sentence formed from one of them.
///
/// The sentences are the two forms a derived "key feature" takes — one naming the route the
/// product is fulfilled through, one naming the category it is classified under — so a
/// response that reports a member's value back as its own content is caught whichever of the
/// two it uses.
fn restatements(categories: &[String], fulfillments: &[String]) -> Vec<String> {
    let mut forms: Vec<String> = Vec::new();
    for category in categories {
        forms.push(category.clone());
        forms.push(format!("Listed under {category}"));
        forms.push(format!("Listed under {category}."));
    }
    for fulfillment in fulfillments {
        forms.push(fulfillment.clone());
        forms.push(format!("Fulfilled as {fulfillment}"));
        forms.push(format!("Fulfilled as {fulfillment}."));
    }
    forms
}

/// The key features one record states, read off a `GetListing` or `GetProduct` response.
fn highlights(body: &Value) -> Vec<String> {
    array(body, "highlights")
        .iter()
        .map(|highlight| highlight.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Assert that the prose of one record — the `case` naming which record and which view — is
/// content of its own and not a restatement of the members beside it.
fn assert_prose_is_own_content(case: &str, body: &Value) {
    let short_description = text(body, "shortDescription");
    let long_description = text(body, "longDescription");
    let categories = display_names(body, "categories");
    let fulfillments = display_names(body, "fulfillmentOptionSummaries");
    let restated = restatements(&categories, &fulfillments);
    let stated = highlights(body);

    assert!(
        !stated.is_empty(),
        "case {case}: the record states at least one key feature, got none in {body}"
    );
    assert!(
        stated.len() <= MAX_HIGHLIGHTS,
        "case {case}: expected at most {MAX_HIGHLIGHTS} highlights, got {}",
        stated.len()
    );
    for (index, highlight) in stated.iter().enumerate() {
        assert!(
            !highlight.trim().is_empty() && highlight.chars().count() <= MAX_HIGHLIGHT_LENGTH,
            "case {case}: highlight {index} must be non-blank and at most \
             {MAX_HIGHLIGHT_LENGTH} characters, got {highlight:?}"
        );
        assert_eq!(
            stated.iter().filter(|other| *other == highlight).count(),
            1,
            "case {case}: highlight {index} is stated more than once, got {stated:?}"
        );
        assert!(
            !restated.contains(highlight),
            "case {case}: highlight {index} restates another member of the same response, \
             expected a key feature of the record, got {highlight:?} beside categories \
             {categories:?} and fulfillment options {fulfillments:?}"
        );
    }

    assert!(
        !long_description.trim().is_empty()
            && long_description.chars().count() <= MAX_LONG_DESCRIPTION_LENGTH,
        "case {case}: the detailed description must be non-blank and at most \
         {MAX_LONG_DESCRIPTION_LENGTH} characters, got {long_description:?}"
    );
    assert_ne!(
        long_description, short_description,
        "case {case}: the detailed description must be prose of its own, got the short \
         description back"
    );
    assert!(
        !long_description.starts_with(&short_description),
        "case {case}: the detailed description must be prose of its own rather than the \
         short description with more text appended, got {long_description:?} for short \
         description {short_description:?}"
    );

    for (index, media) in array(body, "promotionalMedia").iter().enumerate() {
        // The element is a `PromotionalMedia` union, read as the single member it carries so
        // the case covers an image and a video alike.
        let embedded = media
            .as_object()
            .and_then(|object| object.values().next().cloned())
            .unwrap_or_else(|| panic!("case {case}: promotional media {index} must carry one \
                                       media element"));
        let title = text(&embedded, "title");
        assert!(
            !title.trim().is_empty(),
            "case {case}: promotional media {index} must carry a title, got {embedded}"
        );
        // `description` is optional, so a record that captions an element with none carries
        // none and there is nothing to check; a caption it DOES carry says what that element
        // shows rather than repeating a member beside it.
        if let Some(description) = embedded
            .get("description")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        {
            assert!(
                !description.trim().is_empty(),
                "case {case}: promotional media {index} carries a blank caption"
            );
            assert_ne!(
                description, short_description,
                "case {case}: promotional media {index} must be captioned by what it shows, \
                 got the record's short description back"
            );
            assert!(
                !restated.contains(&description),
                "case {case}: promotional media {index} is captioned by restating another \
                 member of the same response, got {description:?}"
            );
        }
    }

    for (index, entry) in array(body, "useCases").iter().enumerate() {
        // The entry wraps one `UseCase`, read as the single member it carries so the case
        // does not restate the wire name of the wrapper.
        let use_case = entry
            .as_object()
            .and_then(|object| object.values().next().cloned())
            .unwrap_or_else(|| panic!("case {case}: use case {index} must carry a use case"));
        for member in ["value", "displayName", "description"] {
            let value = text(&use_case, member);
            assert!(
                !value.trim().is_empty() && value.chars().count() <= MAX_USE_CASE_LENGTH,
                "case {case}: use case {index} member {member} must be non-blank and at most \
                 {MAX_USE_CASE_LENGTH} characters, got {value:?}"
            );
        }
        let description = text(&use_case, "description");
        assert!(
            !restated.contains(&description),
            "case {case}: use case {index} describes the record by restating another member \
             of the same response, got {description:?}"
        );
        assert!(
            !description.starts_with(&short_description),
            "case {case}: use case {index} must describe a scenario of its own rather than \
             the short description with more text appended, got {description:?}"
        );
    }
}

#[tokio::test]
async fn every_records_prose_is_its_own_content() {
    let router = router();
    let listing_ids = catalog_listing_ids(&router).await;
    assert!(
        listing_ids.len() > 25,
        "case setup: the catalog must hold more than one default page of listings, got {}",
        listing_ids.len()
    );

    for listing_id in listing_ids {
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        assert_prose_is_own_content(&format!("GetListing {listing_id}"), &listing);
        for product in products_of(&router, &listing).await {
            let product_id = text(&product, "productId");
            assert_prose_is_own_content(&format!("GetProduct {product_id}"), &product);
        }
    }
}

#[tokio::test]
async fn a_listing_and_its_product_state_the_same_key_features() {
    let router = router();
    for (slug, varies) in ONE_RECORD_ROWS {
        let case = format!("{slug} ({varies})");
        let listing = listing_of_slug(&router, slug).await;
        let listing_highlights = highlights(&listing);
        let products = products_of(&router, &listing).await;
        assert_eq!(
            products.len(),
            1,
            "case {case}: the catalog holds one product for this listing, got {}",
            products.len()
        );
        let product = &products[0];
        assert_eq!(
            highlights(product),
            listing_highlights,
            "case {case}: the product restates the record's key features, expected \
             {listing_highlights:?}, got {:?}",
            highlights(product)
        );
        assert_eq!(
            text(product, "listingId"),
            text(&listing, "listingId"),
            "case {case}: the product must point back at the listing it was reached through"
        );
        assert!(
            !array(&listing, "useCases").is_empty(),
            "case {case}: the record states at least one use case, got none"
        );
    }
}

#[tokio::test]
async fn reading_a_record_twice_returns_the_same_content() {
    let router = router();
    for (slug, varies) in ONE_RECORD_ROWS {
        let case = format!("{slug} ({varies})");
        let first = listing_of_slug(&router, slug).await;
        let listing_id = text(&first, "listingId");
        let second = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        assert_eq!(
            second, first,
            "case {case}: reading {listing_id} again must return the same record"
        );

        let products = products_of(&router, &first).await;
        for product in &products {
            let product_id = text(product, "productId");
            let again = ok(
                &router,
                "getProduct",
                &json!({ "productId": product_id.clone() }),
            )
            .await;
            assert_eq!(
                &again, product,
                "case {case}: reading product {product_id} again must return the same record"
            );
        }
    }
}
