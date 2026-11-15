// SPDX-License-Identifier: CC0-1.0

//! What a drill-down detail view reports about the record the requested id names.
//!
//! The rules under test:
//!
//! * A product's fulfillment is ONE record. `Product` carries a `FulfillmentOptionsList`
//!   and `FulfillmentOption` is a union documented "Each element contains exactly one
//!   fulfillment option type", so every view of that product restates those options and
//!   only those: `fulfillmentOptionSummaries` (on `GetProduct`, on `GetListing` and in a
//!   `SearchListings` summary) reports one summary per distinct type the record offers, and
//!   `ListFulfillmentOptions` serves each option as the union member of its OWN type. A
//!   type the record does not offer appears in no view — so no member that belongs to
//!   another arm (an AMI's `operatingSystems`, `architecture` or `accessUrlTemplate`, say)
//!   is served for a product that is not fulfilled that way.
//! * An optional attribute is claimed only where the record carries it:
//!   `SaasFulfillmentOption$quickLaunch` ("Specifies whether the SaaS product supports
//!   quick-launch deployment") is `ENABLED` only for a record carrying the `launchUrl`
//!   ("The URL that a buyer uses to launch the seller's SaaS product") such a launch sends
//!   the buyer to, and a listing's `QUICK_LAUNCH` badge appears exactly where one of its
//!   products has such an option.
//! * `Category$categoryId` is "The machine-readable identifier of the category" and
//!   `displayName` "The human-readable name of the category", so the identifier is the
//!   catalog's own opaque handle rather than a form of the name it renders: a client can
//!   persist it, read it back unchanged from every view, and pass it back as a `CATEGORY`
//!   filter value to select exactly the listings whose records carry it.
//!
//! How many options a product offers, and the paging over them, is asserted in
//! `fulfillment_option_families`; the deployment status a product's options imply, and the
//! order its categories come back in, in `catalog_record_breadth`.
//!
//! Every request goes through the `MultiServiceRouter`, so the protocol, the routing table
//! and the handlers are all exercised end to end.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_marketplacediscovery_awsmarketplacediscovery::state::category_id;

use crate::catalog_drilldown::{array, ok, router, text, whole_catalog};

/// Every value of the model's `FulfillmentOptionType` enum and the `FulfillmentOption`
/// union member that carries an option of that type.
///
/// Source: `smithy_query(query="shape",shape="FulfillmentOptionType")` for the values and
/// `smithy_query(query="shape",shape="FulfillmentOption")` for the members.
const UNION_MEMBER_OF_TYPE: &[(&str, &str)] = &[
    ("AMAZON_MACHINE_IMAGE", "amazonMachineImageFulfillmentOption"),
    ("API", "apiFulfillmentOption"),
    ("CLOUDFORMATION_TEMPLATE", "cloudFormationFulfillmentOption"),
    ("CONTAINER", "containerFulfillmentOption"),
    ("HELM", "helmFulfillmentOption"),
    ("EKS_ADD_ON", "eksAddOnFulfillmentOption"),
    (
        "EC2_IMAGE_BUILDER_COMPONENT",
        "ec2ImageBuilderComponentFulfillmentOption",
    ),
    ("DATA_EXCHANGE", "dataExchangeFulfillmentOption"),
    (
        "PROFESSIONAL_SERVICES",
        "professionalServicesFulfillmentOption",
    ),
    ("SAAS", "saasFulfillmentOption"),
    ("SAGEMAKER_ALGORITHM", "sageMakerAlgorithmFulfillmentOption"),
    ("SAGEMAKER_MODEL", "sageMakerModelFulfillmentOption"),
];

/// Members only an `AmazonMachineImageFulfillmentOption` declares, which therefore may not
/// be served for a product that offers no AMI fulfillment. (`operatingSystems`,
/// `releaseNotes` and `usageInstructions` are NOT here: other arms declare them too.)
///
/// Source: `smithy_query(query="shape",shape="AmazonMachineImageFulfillmentOption")` read
/// against every other member of the `FulfillmentOption` union.
const AMI_ONLY_MEMBERS: &[&str] = &[
    "architecture",
    "accessUrlTemplate",
    "amiAlias",
    "ebsVolume",
];

/// Categories of the catalog's taxonomy the identifier rule is checked over: one at each of
/// the taxonomy's three levels, under three different top-level categories, and entries the
/// issue that prompted this suite never cited.
///
/// Source: the taxonomy the catalog declares (`state::CATEGORY_TAXONOMY`, read off the AWS
/// Marketplace browse taxonomy), as `(taxonomy key, displayName)`.
const CATEGORY_ROWS: &[(&str, &str)] = &[
    ("infrastructure-software", "Infrastructure Software"),
    ("storage", "Storage"),
    ("data-analytics", "Data Analytics"),
    ("data-integration", "Data Integration"),
    ("iot-edge-computing", "IoT Edge Computing"),
    ("structured-data-ml", "Structured Data"),
    ("training", "Training"),
];

/// Whether `value` has the form of an identifier this catalog issues for a category: the
/// 36-character lower-case hyphenated form, which carries no display text.
fn is_issued_identifier(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok() && value.len() == 36 && value == value.to_lowercase()
}

/// The single union member of a `FulfillmentOption`: its name and its value.
fn union_member(case: &str, option: &Value) -> (String, Value) {
    let object = option
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: a fulfillment option must be an object, got {option}"));
    assert_eq!(
        object.len(),
        1,
        "case {case}: a FulfillmentOption carries exactly one member, got {option}"
    );
    let (name, value) = object.iter().next().expect("the one member");
    (name.clone(), value.clone())
}

/// The `fulfillmentOptionType` of every summary of a record.
fn summarised_types(record: &Value) -> Vec<String> {
    array(record, "fulfillmentOptionSummaries")
        .iter()
        .map(|summary| text(summary, "fulfillmentOptionType"))
        .collect()
}

/// Every fulfillment option of `product_id`, one whole page.
async fn options_of(router: &MultiServiceRouter, product_id: &str) -> Vec<Value> {
    let body = ok(
        router,
        "listFulfillmentOptions",
        &json!({ "productId": product_id, "maxResults": 50 }),
    )
    .await;
    assert!(
        body.get("nextToken").is_none_or(Value::is_null),
        "case {product_id}: no product offers more than one 50-item page, got {body}"
    );
    array(&body, "fulfillmentOptions")
}

/// RULE: every view of a product restates the fulfillment options its record carries, and
/// only those — over every product in the catalog, which differ in option count and type.
#[tokio::test]
async fn every_view_of_a_product_restates_the_options_its_record_carries() {
    let router = router();

    let mut products_seen = 0_usize;
    let mut arms_seen: BTreeSet<String> = BTreeSet::new();
    for summary in whole_catalog(&router).await {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        // The listing is fulfilled through its products, so its summaries are the
        // de-duplicated union of theirs and name no further type.
        let mut listing_types: Vec<String> = Vec::new();

        for entity in array(&listing, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            let detail = ok(
                &router,
                "getProduct",
                &json!({ "productId": product_id.clone() }),
            )
            .await;
            let options = options_of(&router, &product_id).await;
            assert!(
                !options.is_empty(),
                "case {product_id}: a catalog product must report how it is fulfilled"
            );

            // Each option is the union member its own type declares.
            let mut served: Vec<String> = Vec::new();
            for option in &options {
                let (member, body) = union_member(&product_id, option);
                let option_type = text(&body, "fulfillmentOptionType");
                let expected = UNION_MEMBER_OF_TYPE
                    .iter()
                    .find(|(value, _)| *value == option_type)
                    .map(|(_, member)| *member);
                assert_eq!(
                    expected,
                    Some(member.as_str()),
                    "case {product_id}: an option of type {option_type} must be served as its \
                     own union member, got {member}"
                );
                assert!(
                    !text(&body, "fulfillmentOptionDisplayName").trim().is_empty(),
                    "case {product_id}: fulfillmentOptionDisplayName is required and non-blank, \
                     got {body}"
                );
                arms_seen.insert(member);
                if !served.contains(&option_type) {
                    served.push(option_type);
                }
            }

            // The product summarises exactly the types it serves, in the same order.
            assert_eq!(
                summarised_types(&detail),
                served,
                "case getProduct {product_id}: the summaries must restate the types the record \
                 offers, got {:?}",
                summarised_types(&detail)
            );
            for summary in array(&detail, "fulfillmentOptionSummaries") {
                assert!(
                    !text(&summary, "displayName").trim().is_empty(),
                    "case getProduct {product_id}: FulfillmentOptionSummary$displayName is \
                     required and non-blank, got {summary}"
                );
            }

            // A member of another arm is served for no product: an AMI-only member appears
            // only where the record offers AMI fulfillment.
            let offers_ami = served.iter().any(|value| value == "AMAZON_MACHINE_IMAGE");
            for option in &options {
                let (_, body) = union_member(&product_id, option);
                for member in AMI_ONLY_MEMBERS {
                    assert!(
                        offers_ami || body.get(*member).is_none(),
                        "case {product_id} fulfilled as {served:?}: {member} belongs to the AMI \
                         option alone, got {body}"
                    );
                }
            }

            for option_type in served {
                if !listing_types.contains(&option_type) {
                    listing_types.push(option_type);
                }
            }
            products_seen += 1;
        }

        assert_eq!(
            summarised_types(&listing),
            listing_types,
            "case getListing {listing_id}: the listing must summarise the types its products \
             offer, got {:?}",
            summarised_types(&listing)
        );
        assert_eq!(
            summarised_types(&summary),
            listing_types,
            "case searchListings {listing_id}: a summary must report the same types the detail \
             read does, got {:?}",
            summarised_types(&summary)
        );
    }

    assert!(
        products_seen > 10,
        "case the whole catalog: the scan must reach the catalog's products, got \
         {products_seen}"
    );
    assert_eq!(
        arms_seen.len(),
        UNION_MEMBER_OF_TYPE.len(),
        "case the whole catalog: every modelled fulfillment arm must stay reachable, got \
         {arms_seen:?}"
    );
}

/// RULE: quick launch is claimed only where the record carries the launch URL such a launch
/// uses, and the listing badge that restates it appears exactly there.
#[tokio::test]
async fn quick_launch_is_claimed_only_where_the_record_carries_a_launch_url() {
    let router = router();

    let mut enabled = 0_usize;
    let mut disabled = 0_usize;
    for summary in whole_catalog(&router).await {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        let mut quick_launchable = false;
        for entity in array(&listing, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            for option in options_of(&router, &product_id).await {
                let (member, body) = union_member(&product_id, &option);
                if member != "saasFulfillmentOption" {
                    continue;
                }
                let quick_launch = text(&body, "quickLaunch");
                // Source: `smithy_query(query="shape",shape="SaasQuickLaunchStatus")`.
                assert!(
                    ["ENABLED", "DISABLED"].contains(&quick_launch.as_str()),
                    "case {product_id}: quickLaunch must be a modelled status, got \
                     {quick_launch:?}"
                );
                if quick_launch == "ENABLED" {
                    assert!(
                        body.get("launchUrl").is_some(),
                        "case {product_id}: a quick-launchable option must carry the launchUrl \
                         such a launch uses, got {body}"
                    );
                    quick_launchable = true;
                    enabled += 1;
                } else {
                    disabled += 1;
                }
            }
        }
        let badges: BTreeSet<String> = array(&listing, "badges")
            .iter()
            .map(|badge| text(badge, "badgeType"))
            .collect();
        assert_eq!(
            badges.contains("QUICK_LAUNCH"),
            quick_launchable,
            "case getListing {listing_id}: the QUICK_LAUNCH badge must restate whether a \
             product of the listing supports it, got {badges:?}"
        );
    }

    assert!(
        enabled > 0 && disabled > 0,
        "case the whole catalog: both quick-launch statuses must stay reachable, got {enabled} \
         enabled and {disabled} disabled"
    );
}

/// RULE: a category is named on the wire by the identifier the catalog issues for it — an
/// opaque identifier rather than a form of its display name — and that identifier is what a
/// `CATEGORY` filter selects by.
#[tokio::test]
async fn a_category_is_named_by_an_opaque_identifier_that_round_trips_as_a_filter() {
    let router = router();

    // Which listings carry which category, read off the detail view of every listing.
    let mut carriers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for summary in whole_catalog(&router).await {
        let listing_id = text(&summary, "listingId");
        let listing = ok(
            &router,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        for category in array(&listing, "categories") {
            let id = text(&category, "categoryId");
            let display_name = text(&category, "displayName");
            assert!(
                is_issued_identifier(&id),
                "case getListing {listing_id}: categoryId must be an identifier this service \
                 issues, got {id:?} for {display_name:?}"
            );
            assert_ne!(
                id, display_name,
                "case getListing {listing_id}: the identifier and the display name are \
                 different members"
            );
            names.insert(id.clone(), display_name);
            carriers.entry(id).or_default().insert(listing_id.clone());
        }
    }

    for (key, display_name) in CATEGORY_ROWS {
        let id = category_id(key);
        assert!(
            is_issued_identifier(&id),
            "case {key}: the issued identifier must have the form this service issues, got {id:?}"
        );
        // It carries none of the display name: not the name itself, not a case-folded or
        // hyphenated form of it, and none of its words.
        let folded = display_name.to_lowercase();
        assert_ne!(id, folded.replace(' ', "-"), "case {key}: the identifier must not be a slug of {display_name:?}");
        for word in folded.split(' ').filter(|word| word.len() > 2) {
            assert!(
                !id.contains(word),
                "case {key}: the identifier must carry none of the display name, got {id:?} \
                 containing {word:?}"
            );
        }
        assert_eq!(
            names.get(&id).map(String::as_str),
            Some(*display_name),
            "case {key}: the catalog must report {display_name:?} for {id}, got {:?}",
            names.get(&id)
        );

        // Passing it back as a `CATEGORY` filter value selects exactly the listings whose
        // records carry it.
        let body = ok(
            &router,
            "searchListings",
            &json!({
                "maxResults": 100,
                "filters": [{ "filterType": "CATEGORY", "filterValues": [id.clone()] }],
            }),
        )
        .await;
        let selected: BTreeSet<String> = array(&body, "listingSummaries")
            .iter()
            .map(|summary| text(summary, "listingId"))
            .collect();
        let expected = carriers.get(&id).cloned().unwrap_or_default();
        assert!(
            !expected.is_empty(),
            "case {key}: the catalog must hold a listing classified under it"
        );
        assert_eq!(
            selected, expected,
            "case CATEGORY={id} ({key}): the filter must select the listings whose records \
             carry it, got {selected:?}"
        );

        // And a `CATEGORY` facet reports the same identifier, so the value a client refines
        // by is the one the records carry.
        let facets = ok(
            &router,
            "searchFacets",
            &json!({ "facetTypes": ["CATEGORY"] }),
        )
        .await;
        let reported: BTreeSet<String> = facets
            .get("listingFacets")
            .and_then(Value::as_object)
            .and_then(|map| map.get("CATEGORY"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|facet| text(facet, "value"))
            .collect();
        assert!(
            reported.contains(&id),
            "case {key}: the CATEGORY facet must report {id}, got {reported:?}"
        );
    }
}
