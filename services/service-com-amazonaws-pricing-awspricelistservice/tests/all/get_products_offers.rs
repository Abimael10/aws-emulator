// SPDX-License-Identifier: CC0-1.0

//! `GetProducts` answers a filtered query with the offer documents of the products the
//! filters select.
//!
//! The rule under test, over a TABLE of queries rather than one recorded request: for
//! every product the catalog publishes, a filter set naming that product's attribute
//! values is answered with an `aws_v1` offer document that DESCRIBES that product — its
//! own attributes and SKU — priced under the terms it is offered under, with each price
//! dimension stating the rate it charges and the usage it prices; and a matched set
//! longer than the requested page is carried on a `NextToken`.
//!
//! The same rule is also stated for the price lookup a cost-estimation client makes:
//! a filter set pinning one published value of every attribute that identifies an
//! on-demand compute SKU selects exactly ONE product, in EVERY Region the catalog
//! publishes and whichever of the two attributes names that Region.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::pricing_support::{
    all_attribute_values, all_offer_documents, offer_attribute, offer_documents, ok, product_sku,
    product_skus, router,
};

/// The `licenseModel` of an operating system that needs no license, as
/// `GetAttributeValues(AmazonEC2, licenseModel)` publishes it.
const NO_LICENSE_REQUIRED: &str = "No License required";

/// The `preInstalledSw` of an instance carrying no pre-installed software, as
/// `GetAttributeValues(AmazonEC2, preInstalledSw)` publishes it.
const NO_PRE_INSTALLED_SW: &str = "NA";

/// A `TERM_MATCH` filter per `(field, value)` pair: the comparison the `FilterType`
/// member documents for an exact value.
fn term_matches(pairs: &[(&str, &str)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(field, value)| json!({ "Type": "TERM_MATCH", "Field": field, "Value": value }))
            .collect(),
    )
}

/// The rate a price dimension charges, as its `pricePerUnit.USD` states it.
fn dimension_rate(dimension: &Value) -> String {
    dimension
        .pointer("/pricePerUnit/USD")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The same rate written the way a description writes it: without the trailing zeros of
/// the published ten-place scale, keeping one decimal place.
fn display_rate(rate: &str) -> String {
    let trimmed = rate.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.to_string()
    }
}

/// One term block's `(offer term code, term)` pairs.
fn terms_of(document: &Value, term_type: &str) -> Vec<(String, Value)> {
    document
        .pointer(&format!("/terms/{term_type}"))
        .and_then(Value::as_object)
        .map(|by_code| {
            by_code
                .iter()
                .map(|(code, term)| (code.clone(), term.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// One term's `(rate code, dimension)` pairs.
fn dimensions_of(term: &Value) -> Vec<(String, Value)> {
    term.get("priceDimensions")
        .and_then(Value::as_object)
        .map(|by_code| {
            by_code
                .iter()
                .map(|(code, dimension)| (code.clone(), dimension.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// One price lookup: the values it pins for the attributes that identify an on-demand
/// compute SKU.
#[derive(Clone, Copy)]
struct OnDemandLookup<'a> {
    instance_type: &'a str,
    operating_system: &'a str,
    license_model: &'a str,
    /// `location` (the Region display name) or `regionCode` — the two attributes a
    /// product names its Region by.
    location_field: &'a str,
    location_value: &'a str,
    tenancy: &'a str,
    capacity_status: &'a str,
}

impl<'a> OnDemandLookup<'a> {
    /// The attribute values this lookup pins.
    fn pinned(&self) -> Vec<(&'a str, &'a str)> {
        vec![
            ("instanceType", self.instance_type),
            ("operatingSystem", self.operating_system),
            (self.location_field, self.location_value),
            ("tenancy", self.tenancy),
            ("preInstalledSw", NO_PRE_INSTALLED_SW),
            ("capacitystatus", self.capacity_status),
            ("licenseModel", self.license_model),
        ]
    }

    /// Those values as the `TERM_MATCH` filter set a client sends.
    fn filters(&self) -> Value {
        term_matches(&self.pinned())
    }
}

/// The filters that select the on-demand compute products of one Region under one
/// operating system, leaving the instance type, the tenancy and the capacity status
/// free.
fn region_compute_filters(location: &str, operating_system: &str, license_model: &str) -> Value {
    term_matches(&[
        ("operatingSystem", operating_system),
        ("location", location),
        ("preInstalledSw", NO_PRE_INSTALLED_SW),
        ("licenseModel", license_model),
    ])
}

/// Every `location` the compute catalog publishes.
///
/// Enumerated through `GetAttributeValues(AmazonEC2, location)` — the operation that
/// publishes an attribute's value set — so the rows below are the catalog's own Regions
/// rather than a hand-written list.
async fn published_locations(router: &MultiServiceRouter) -> Vec<String> {
    let mut locations = all_attribute_values(
        router,
        &json!({ "ServiceCode": "AmazonEC2", "AttributeName": "location" }),
    )
    .await;
    locations.sort();
    locations.dedup();
    locations
}

/// The offer documents a `GetProducts` query selects, paged to the end.
async fn selected(router: &MultiServiceRouter, filters: &Value) -> Vec<Value> {
    all_offer_documents(
        router,
        &json!({ "ServiceCode": "AmazonEC2", "Filters": filters }),
    )
    .await
}

#[tokio::test]
async fn the_on_demand_lookup_selects_one_sku_in_every_published_location() {
    let router = router();

    // Rows: EVERY Region the catalog publishes compute prices in, not one of them.
    let locations = published_locations(&router).await;
    assert!(
        locations.len() > 10,
        "the catalog must publish compute prices in more Regions than a handful, got \
         {locations:?}"
    );
    // Two display names the enumeration must carry, so a Region named by the catalog is
    // reachable by the name it publishes.
    for expected in ["US East (N. Virginia)", "US West (Oregon)"] {
        assert!(
            locations.contains(&expected.to_string()),
            "case {expected}: the enumeration must carry this published Region, got \
             {locations:?}"
        );
    }

    // No two lookups may land on the same product: the pinned attributes identify a SKU.
    let mut selected_by_case: BTreeMap<String, String> = BTreeMap::new();

    for location in &locations {
        // The Region's own Linux on-demand products, paged to the end: the instance
        // types sampled below are the ones THIS Region publishes, so no row names a
        // type the catalog does not price there.
        let published = selected(
            &router,
            &term_matches(&[
                ("operatingSystem", "Linux"),
                ("location", location.as_str()),
                ("tenancy", "Shared"),
                ("capacitystatus", "Used"),
                ("preInstalledSw", NO_PRE_INSTALLED_SW),
                ("licenseModel", NO_LICENSE_REQUIRED),
            ]),
        )
        .await;
        let by_type: BTreeMap<String, String> = published
            .iter()
            .filter_map(|document| {
                Some((
                    offer_attribute(document, "instanceType")?,
                    product_sku(document),
                ))
            })
            .collect();
        assert!(
            !by_type.is_empty(),
            "case {location}: a Region the catalog publishes products in must price \
             on-demand compute there, got {} products",
            published.len()
        );
        assert_eq!(
            by_type.len(),
            published.len(),
            "case {location}: leaving only the instance type free must leave one SKU per \
             published type, got {} SKUs over {} types",
            published.len(),
            by_type.len()
        );

        // Three sizes per Region — the smallest, a middle one and the largest the Region
        // publishes — so the rule is read off more than one instance family.
        let types: Vec<&String> = by_type.keys().collect();
        let samples: BTreeSet<usize> =
            BTreeSet::from([0, types.len() / 2, types.len().saturating_sub(1)]);
        for index in samples {
            let instance_type = types[index].as_str();
            let case = format!("{location}/{instance_type}");
            let lookup = OnDemandLookup {
                instance_type,
                operating_system: "Linux",
                license_model: NO_LICENSE_REQUIRED,
                location_field: "location",
                location_value: location.as_str(),
                tenancy: "Shared",
                capacity_status: "Used",
            };

            let response = ok(
                &router,
                "GetProducts",
                &json!({
                    "ServiceCode": "AmazonEC2",
                    "Filters": lookup.filters(),
                    "MaxResults": 10,
                }),
            )
            .await;
            let documents = offer_documents(&response);
            assert_eq!(
                documents.len(),
                1,
                "case {case}: the lookup must select exactly one SKU, got {}: {:?}",
                documents.len(),
                product_skus(&response)
            );
            let document = &documents[0];
            let sku = product_sku(document);

            // The selected product echoes every attribute the lookup pinned, and is the
            // same product the Region's own listing reports for that type.
            for (field, value) in lookup.pinned() {
                assert_eq!(
                    offer_attribute(document, field).as_deref(),
                    Some(value),
                    "case {case}: the selected SKU must carry {field}={value}, got \
                     {document}"
                );
            }
            assert_eq!(
                by_type.get(instance_type),
                Some(&sku),
                "case {case}: the lookup must select the SKU the Region's listing reports, \
                 got {sku}"
            );
            assert_eq!(
                document
                    .pointer("/product/productFamily")
                    .and_then(Value::as_str),
                Some("Compute Instance"),
                "case {case}: a priced instance is a compute instance, got {document}"
            );

            // The hourly on-demand rate a cost estimate reads is there and positive.
            let on_demand = terms_of(document, "OnDemand");
            assert_eq!(
                on_demand.len(),
                1,
                "case {case}: an instance is offered on demand once, got {on_demand:?}"
            );
            let dimensions = dimensions_of(&on_demand[0].1);
            assert_eq!(
                dimensions.len(),
                1,
                "case {case}: the on-demand term prices one dimension, got {dimensions:?}"
            );
            let (_, dimension) = &dimensions[0];
            assert_eq!(
                dimension.get("unit").and_then(Value::as_str),
                Some("Hrs"),
                "case {case}: on-demand instance usage is priced per hour, got {dimension}"
            );
            let rate = dimension_rate(dimension);
            assert!(
                rate.parse::<f64>().is_ok_and(|rate| rate > 0.0),
                "case {case}: expected a positive USD on-demand rate, got {rate:?}"
            );

            // Naming the same Region by the other attribute it publishes selects the
            // same SKU.
            let region_code = offer_attribute(document, "regionCode").unwrap_or_default();
            assert!(
                !region_code.is_empty(),
                "case {case}: a Region-scoped product must name its Region code, got \
                 {document}"
            );
            let by_region_code = OnDemandLookup {
                location_field: "regionCode",
                location_value: region_code.as_str(),
                ..lookup
            };
            let same = ok(
                &router,
                "GetProducts",
                &json!({
                    "ServiceCode": "AmazonEC2",
                    "Filters": by_region_code.filters(),
                    "MaxResults": 10,
                }),
            )
            .await;
            assert_eq!(
                product_skus(&same),
                vec![sku.clone()],
                "case {case}: naming the Region by regionCode={region_code} must select the \
                 same SKU, got {:?}",
                product_skus(&same)
            );

            if let Some(previous) = selected_by_case.insert(sku.clone(), case.clone()) {
                panic!(
                    "case {case}: the pinned attributes must identify one SKU of its own, \
                     but {sku} was already selected by case {previous}"
                );
            }
        }
    }
}

#[tokio::test]
async fn tenancy_and_capacity_status_separate_a_regions_remaining_skus() {
    let router = router();

    // The value sets the catalog publishes for the two attributes, read off the API
    // rather than written out here.
    let tenancies = all_attribute_values(
        &router,
        &json!({ "ServiceCode": "AmazonEC2", "AttributeName": "tenancy" }),
    )
    .await;
    let capacity_statuses = all_attribute_values(
        &router,
        &json!({ "ServiceCode": "AmazonEC2", "AttributeName": "capacitystatus" }),
    )
    .await;
    assert!(
        tenancies.len() > 1 && capacity_statuses.len() > 1,
        "the catalog must price more than one tenancy and capacity status, got \
         {tenancies:?} and {capacity_statuses:?}"
    );

    // Rows: the first and the last Region the catalog publishes, so the rule is not read
    // off the one Region the other suites pin.
    let locations = published_locations(&router).await;
    let sampled: BTreeSet<&String> = [locations.first(), locations.last()]
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(
        sampled.len(),
        2,
        "the catalog must publish more than one Region, got {locations:?}"
    );

    for location in sampled {
        let region_products = selected(
            &router,
            &region_compute_filters(location, "Linux", NO_LICENSE_REQUIRED),
        )
        .await;
        let instance_type = region_products
            .iter()
            .filter_map(|document| offer_attribute(document, "instanceType"))
            .min()
            .unwrap_or_else(|| panic!("case {location}: the Region must price compute"));
        let case = format!("{location}/{instance_type}");

        // Every SKU this Region publishes for that type and operating system: the set
        // the two attributes have to separate.
        let expected: BTreeSet<String> = region_products
            .iter()
            .filter(|document| {
                offer_attribute(document, "instanceType").as_deref() == Some(&instance_type)
            })
            .map(product_sku)
            .collect();
        assert!(
            expected.len() > 1,
            "case {case}: the Region must publish more than one SKU for a type, got \
             {expected:?}"
        );

        let mut covered: BTreeSet<String> = BTreeSet::new();
        for tenancy in &tenancies {
            for capacity_status in &capacity_statuses {
                let lookup = OnDemandLookup {
                    instance_type: instance_type.as_str(),
                    operating_system: "Linux",
                    license_model: NO_LICENSE_REQUIRED,
                    location_field: "location",
                    location_value: location.as_str(),
                    tenancy: tenancy.as_str(),
                    capacity_status: capacity_status.as_str(),
                };
                let skus: Vec<String> = selected(&router, &lookup.filters())
                    .await
                    .iter()
                    .map(product_sku)
                    .collect();
                assert!(
                    skus.len() <= 1,
                    "case {case} {tenancy}/{capacity_status}: the pinned attributes must \
                     identify at most one SKU, got {skus:?}"
                );
                covered.extend(skus);
            }
        }
        assert_eq!(
            covered, expected,
            "case {case}: the published tenancy and capacity statuses must account for \
             every SKU of that type, got {covered:?}"
        );
    }
}

#[tokio::test]
async fn a_filtered_query_answers_with_the_offer_documents_of_the_products_it_selects() {
    let router = router();

    // Rows differ in service, product family, how many attributes the filter set pins
    // and how many products it selects: a compute instance by instance type and
    // operating system (one per Region), the same instance pinned to one Region, a
    // Windows instance, a storage volume, an object-storage class and a service priced
    // with one usage type per Region.
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "AmazonEC2",
            &[
                ("instanceType", "t3.nano"),
                ("operatingSystem", "Linux"),
                ("tenancy", "Shared"),
                ("capacitystatus", "Used"),
                ("preInstalledSw", "NA"),
            ],
        ),
        (
            "AmazonEC2",
            &[
                ("instanceType", "t3.micro"),
                ("operatingSystem", "Linux"),
                ("location", "US East (N. Virginia)"),
            ],
        ),
        (
            "AmazonEC2",
            &[("instanceType", "m5.large"), ("operatingSystem", "Windows")],
        ),
        (
            "AmazonEC2",
            &[("instanceType", "m6g.large"), ("operatingSystem", "Linux")],
        ),
        ("AmazonEC2", &[("volumeApiName", "gp3")]),
        ("AmazonS3", &[("storageClass", "Archive")]),
        ("AWSLambda", &[("regionCode", "us-east-1")]),
    ];

    for (service_code, filters) in cases.iter().copied() {
        let case = format!("{service_code} {filters:?}");
        let response = ok(
            &router,
            "GetProducts",
            &json!({
                "ServiceCode": service_code,
                "Filters": term_matches(filters),
                "MaxResults": 100,
            }),
        )
        .await;
        assert_eq!(
            response.get("FormatVersion").and_then(Value::as_str),
            Some("aws_v1"),
            "case {case}: the answer must report the catalog format version, got {response}"
        );
        let documents = offer_documents(&response);
        assert!(
            !documents.is_empty(),
            "case {case}: a filter set naming values the catalog publishes must select at \
             least one product, got {response}"
        );

        for document in &documents {
            let sku = product_sku(document);
            // The document describes the product the filters selected.
            for (field, value) in filters.iter().copied() {
                assert_eq!(
                    offer_attribute(document, field).as_deref(),
                    Some(value),
                    "case {case}: {sku} must carry {field}={value}, got {document}"
                );
            }
            assert_eq!(
                document.get("serviceCode").and_then(Value::as_str),
                Some(service_code),
                "case {case}: {sku} must name its service, got {document}"
            );
            assert!(
                document
                    .pointer("/product/productFamily")
                    .and_then(Value::as_str)
                    .is_some_and(|family| !family.is_empty()),
                "case {case}: {sku} must report a product family, got {document}"
            );

            // Every term prices THIS product, is keyed by the SKU and its own offer term
            // code, and each of its dimensions states the rate it charges, keyed by the
            // rate code derived from the term's key.
            let term_types: BTreeSet<String> = document
                .get("terms")
                .and_then(Value::as_object)
                .map(|terms| terms.keys().cloned().collect())
                .unwrap_or_default();
            assert!(
                term_types.contains("OnDemand"),
                "case {case}: {sku} must be priced on demand, got {term_types:?}"
            );
            for term_type in &term_types {
                for (code, term) in terms_of(document, term_type) {
                    assert_eq!(
                        term.get("sku").and_then(Value::as_str),
                        Some(sku.as_str()),
                        "case {case}: {term_type} term {code} must price {sku}, got {term}"
                    );
                    let offer_term_code = term
                        .get("offerTermCode")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    assert_eq!(
                        code,
                        format!("{sku}.{offer_term_code}"),
                        "case {case}: a term is keyed by SKU and offer term code, got {code}"
                    );
                    let dimensions = dimensions_of(&term);
                    assert!(
                        !dimensions.is_empty(),
                        "case {case}: {term_type} term {code} must carry price dimensions, \
                         got {term}"
                    );
                    for (rate_code, dimension) in dimensions {
                        assert!(
                            rate_code.starts_with(&format!("{code}.")),
                            "case {case}: a rate code extends its term key, got {rate_code}"
                        );
                        assert_eq!(
                            dimension.get("rateCode").and_then(Value::as_str),
                            Some(rate_code.as_str()),
                            "case {case}: a dimension must state its own rate code, got \
                             {dimension}"
                        );
                        assert!(
                            dimension
                                .get("unit")
                                .and_then(Value::as_str)
                                .is_some_and(|unit| !unit.is_empty()),
                            "case {case}: {rate_code} must state the unit it prices, got \
                             {dimension}"
                        );
                        assert!(
                            dimension_rate(&dimension).parse::<f64>().is_ok(),
                            "case {case}: {rate_code} must state a USD rate, got {dimension}"
                        );
                    }
                }
            }
        }

        // A read does not change what a later caller sees.
        let again = ok(
            &router,
            "GetProducts",
            &json!({
                "ServiceCode": service_code,
                "Filters": term_matches(filters),
                "MaxResults": 100,
            }),
        )
        .await;
        assert_eq!(
            response, again,
            "case {case}: the same query must answer the same way"
        );
    }
}

#[tokio::test]
async fn a_compute_offer_prices_the_instance_it_describes() {
    let router = router();

    // Rows differ in instance family, size and operating system. `factor` is the
    // normalized-unit factor AWS publishes for the size (the documented table: nano
    // 0.25, micro 0.5, small 1, medium 2, large 4, xlarge 8, and `<n>xlarge` n times
    // the xlarge factor) — including sizes beyond the ones the queries above name.
    let cases: &[(&str, &str, &str)] = &[
        ("t3.nano", "Linux", "0.25"),
        ("t3.micro", "Linux", "0.5"),
        ("t3.small", "Linux", "1"),
        ("t3.medium", "Linux", "2"),
        ("m5.large", "Linux", "4"),
        ("m5.xlarge", "Linux", "8"),
        ("m5.2xlarge", "Linux", "16"),
        ("m5.large", "Windows", "4"),
        ("t3.nano", "Windows", "0.25"),
    ];

    for (instance_type, operating_system, factor) in cases.iter().copied() {
        let case = format!("{instance_type}/{operating_system}");
        let response = ok(
            &router,
            "GetProducts",
            &json!({
                "ServiceCode": "AmazonEC2",
                "Filters": term_matches(&[
                    ("instanceType", instance_type),
                    ("operatingSystem", operating_system),
                    ("location", "US East (N. Virginia)"),
                    ("tenancy", "Shared"),
                    ("capacitystatus", "Used"),
                    ("preInstalledSw", "NA"),
                ]),
            }),
        )
        .await;
        let documents = offer_documents(&response);
        assert_eq!(
            documents.len(),
            1,
            "case {case}: the filter set must select exactly one priced SKU, got {}",
            documents.len()
        );
        let document = &documents[0];
        let sku = product_sku(document);

        assert_eq!(
            offer_attribute(document, "normalizationSizeFactor").as_deref(),
            Some(factor),
            "case {case}: expected the published normalization factor {factor}, got \
             {document}"
        );
        assert_eq!(
            offer_attribute(document, "usagetype").as_deref(),
            Some(format!("BoxUsage:{instance_type}").as_str()),
            "case {case}: the usage type must name the instance type, got {document}"
        );
        assert_eq!(
            document
                .pointer("/product/productFamily")
                .and_then(Value::as_str),
            Some("Compute Instance"),
            "case {case}: a priced instance is a compute instance, got {document}"
        );

        // The on-demand term is charged per instance hour, and its description states
        // the rate it charges and the instance it prices.
        let on_demand = terms_of(document, "OnDemand");
        assert_eq!(
            on_demand.len(),
            1,
            "case {case}: an instance is offered on demand once, got {on_demand:?}"
        );
        let dimensions = dimensions_of(&on_demand[0].1);
        assert_eq!(
            dimensions.len(),
            1,
            "case {case}: the on-demand term prices one dimension, got {dimensions:?}"
        );
        let (_, dimension) = &dimensions[0];
        assert_eq!(
            dimension.get("unit").and_then(Value::as_str),
            Some("Hrs"),
            "case {case}: on-demand instance usage is priced per hour, got {dimension}"
        );
        let rate = dimension_rate(dimension);
        assert!(
            rate.parse::<f64>().is_ok_and(|rate| rate > 0.0),
            "case {case}: expected a positive on-demand rate, got {rate:?}"
        );
        assert_eq!(
            dimension.get("description").and_then(Value::as_str),
            Some(
                format!(
                    "${} per On Demand {operating_system} {instance_type} Instance Hour",
                    display_rate(&rate)
                )
                .as_str()
            ),
            "case {case}: the description must state the rate and the instance it prices, \
             got {dimension}"
        );

        // The reservations it is also offered under price the same usage below the
        // on-demand rate, and an option that pays part of the lease up front states that
        // fee as its own dimension.
        let reserved = terms_of(document, "Reserved");
        assert!(
            reserved.len() > 1,
            "case {case}: an instance is offered under more than one reservation, got \
             {reserved:?}"
        );
        let on_demand_rate: f64 = rate.parse().expect("a parsable on-demand rate");
        for (code, term) in &reserved {
            let option = term
                .pointer("/termAttributes/PurchaseOption")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let lease = term
                .pointer("/termAttributes/LeaseContractLength")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                lease == "1yr" || lease == "3yr",
                "case {case}/{code}: expected a published lease length, got {lease:?}"
            );
            let mut usage_dimensions = 0usize;
            let mut fee_dimensions = 0usize;
            for (rate_code, dimension) in dimensions_of(term) {
                let unit = dimension
                    .get("unit")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let amount: f64 = dimension_rate(&dimension)
                    .parse()
                    .expect("a parsable reserved rate");
                if unit == "Hrs" {
                    usage_dimensions += 1;
                    assert!(
                        amount < on_demand_rate,
                        "case {case}/{code}: a reservation must price usage below the \
                         on-demand {on_demand_rate}, got {amount}"
                    );
                } else {
                    fee_dimensions += 1;
                    assert_eq!(
                        unit, "Quantity",
                        "case {case}/{rate_code}: a one-off fee is a quantity, got {dimension}"
                    );
                    assert_eq!(
                        dimension.get("description").and_then(Value::as_str),
                        Some("Upfront Fee"),
                        "case {case}/{rate_code}: expected the upfront-fee description, got \
                         {dimension}"
                    );
                    assert!(
                        amount > 0.0,
                        "case {case}/{rate_code}: an upfront fee must be positive, got {amount}"
                    );
                }
            }
            assert_eq!(
                usage_dimensions, 1,
                "case {case}/{code}: a reserved term prices its usage once"
            );
            assert_eq!(
                fee_dimensions,
                usize::from(option != "No Upfront"),
                "case {case}/{code}: {option} must carry a fee dimension only when it pays \
                 one"
            );
        }
        assert_eq!(
            document
                .pointer("/terms/Reserved")
                .and_then(Value::as_object)
                .map(serde_json::Map::len),
            Some(reserved.len()),
            "case {case}: {sku} must publish each reservation once"
        );
    }
}

#[tokio::test]
async fn a_matched_set_longer_than_the_page_is_carried_on_a_next_token() {
    let router = router();

    // The instance-vocabulary query: an instance type the catalog prices in every
    // Region, so the matched set spans several pages. Rows differ in page size,
    // including the published bounds of MaxResults (1..100) and the whole set in one
    // page.
    let filters = term_matches(&[
        ("instanceType", "t3.nano"),
        ("operatingSystem", "Linux"),
        ("tenancy", "Shared"),
        ("capacitystatus", "Used"),
        ("preInstalledSw", "NA"),
    ]);

    let whole = ok(
        &router,
        "GetProducts",
        &json!({ "ServiceCode": "AmazonEC2", "Filters": filters, "MaxResults": 100 }),
    )
    .await;
    let all: Vec<String> = offer_documents(&whole).iter().map(product_sku).collect();
    assert!(
        all.len() > 10,
        "the catalog must price this instance type in more Regions than one small page, \
         got {}",
        all.len()
    );
    assert!(
        whole.get("NextToken").is_none_or(Value::is_null),
        "case MaxResults=100: a single page must carry no NextToken, got {whole}"
    );

    for page_size in [1_i64, 2, 5, 10] {
        let mut collected: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut body = json!({
                "ServiceCode": "AmazonEC2",
                "Filters": filters,
                "MaxResults": page_size,
            });
            if let Some(token) = &next_token {
                body["NextToken"] = json!(token);
            }
            let page = ok(&router, "GetProducts", &body).await;
            let skus: Vec<String> = offer_documents(&page).iter().map(product_sku).collect();
            assert!(
                skus.len() <= usize::try_from(page_size).expect("a small page size"),
                "case MaxResults={page_size} page {pages}: a page must not exceed the page \
                 size, got {}",
                skus.len()
            );
            collected.extend(skus);
            pages += 1;
            next_token = page
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            // The token is present for exactly as long as products remain unreturned.
            assert_eq!(
                next_token.is_some(),
                collected.len() < all.len(),
                "case MaxResults={page_size} page {pages}: expected a NextToken while \
                 {} of {} products remain, got {page}",
                all.len().saturating_sub(collected.len()),
                all.len()
            );
            if next_token.is_none() || pages > all.len() {
                break;
            }
        }
        assert_eq!(
            collected, all,
            "case MaxResults={page_size}: paging must yield every matched product once, in \
             order"
        );
    }
}

// ---------------------------------------------------------------------------
// The CONTENT of a Compute Instance offer document: the instance's whole published
// shape, the wording of its reserved terms, and the publication it is dated at.
// ---------------------------------------------------------------------------

/// The attributes a Compute Instance offer document publishes about the instance's own
/// hardware and platform, beside the ones that identify the SKU.
///
/// A cost or right-sizing client reads these off the document — the market option and the
/// availability zone to tell an on-demand Region-scoped offer from a spot or zonal one,
/// the bandwidth, storage, clock and compute-unit figures to compare shapes, and the
/// instruction-set flags to decide fitness — and every one of them is also a `GetProducts`
/// filter field, so a document that omits one answers both lookups with nothing.
const SHAPE_ATTRIBUTES: &[&str] = &[
    "availabilityzone",
    "classicnetworkingsupport",
    "clockSpeed",
    "dedicatedEbsThroughput",
    "dedicatedEbsThroughputDescription",
    "ecu",
    "enhancedNetworkingSupported",
    "gpuMemory",
    "instanceFamilyCategory",
    "intelAvx2Available",
    "intelAvxAvailable",
    "intelTurboAvailable",
    "marketoption",
    "networkPerformance",
    "processorArchitecture",
    "processorFeatures",
    "storage",
    "vpcnetworkingsupport",
];

/// The attributes that identify one compute SKU, which the document publishes beside the
/// shape above.
const IDENTITY_ATTRIBUTES: &[&str] = &[
    "capacitystatus",
    "currentGeneration",
    "instanceFamily",
    "instanceType",
    "licenseModel",
    "location",
    "locationType",
    "memory",
    "normalizationSizeFactor",
    "operatingSystem",
    "operation",
    "physicalProcessor",
    "preInstalledSw",
    "regionCode",
    "servicecode",
    "servicename",
    "tenancy",
    "usagetype",
    "vcpu",
];

/// One compute SKU, named the way a price lookup names it.
#[derive(Clone, Copy)]
struct ComputeSku<'a> {
    instance_type: &'a str,
    operating_system: &'a str,
    tenancy: &'a str,
    capacity_status: &'a str,
}

impl<'a> ComputeSku<'a> {
    /// The shared, in-use SKU of one instance type and platform — what an on-demand
    /// price lookup asks for.
    const fn in_use(instance_type: &'a str, operating_system: &'a str) -> Self {
        Self {
            instance_type,
            operating_system,
            tenancy: "Shared",
            capacity_status: "Used",
        }
    }
}

/// The one offer document of one compute SKU in the Region the catalog prices the whole
/// fleet in.
async fn compute_offer(router: &MultiServiceRouter, sku: ComputeSku<'_>) -> Value {
    let case = format!(
        "{}/{}/{}/{}",
        sku.instance_type, sku.operating_system, sku.tenancy, sku.capacity_status
    );
    let response = ok(
        router,
        "GetProducts",
        &json!({
            "ServiceCode": "AmazonEC2",
            "Filters": term_matches(&[
                ("instanceType", sku.instance_type),
                ("operatingSystem", sku.operating_system),
                ("regionCode", "us-east-1"),
                ("tenancy", sku.tenancy),
                ("capacitystatus", sku.capacity_status),
                ("preInstalledSw", NO_PRE_INSTALLED_SW),
            ]),
        }),
    )
    .await;
    let documents = offer_documents(&response);
    assert_eq!(
        documents.len(),
        1,
        "case {case}: the filter set must select exactly one priced SKU, got {}",
        documents.len()
    );
    documents[0].clone()
}

/// One attribute of an offer document, or an empty string when it carries none — so an
/// assertion can name the attribute that is missing rather than panic.
fn attribute_or_empty(document: &Value, name: &str) -> String {
    offer_attribute(document, name).unwrap_or_default()
}

/// The leading figure of a bandwidth attribute (`"43 Mbps"` is 43), or `None` when it
/// states no figure.
fn leading_figure(text: &str) -> Option<u64> {
    text.split_whitespace()
        .find_map(|word| word.parse::<u64>().ok())
}

/// One expectation about an instance type whose shape is unlike the rest.
struct ShapeCase<'a> {
    instance_type: &'a str,
    operating_system: &'a str,
    /// `storage`: the instance store AWS publishes for that size, or `EBS only` for a
    /// type with none.
    storage: &'a str,
    /// `gpuMemory`: the memory of the type's GPUs, or `NA` for a type with none.
    gpu_memory: &'a str,
    /// `processorFeatures`: the instruction-set extensions published for the type's
    /// processor, or `NA` for a part the API publishes none for.
    processor_features: &'a str,
}

#[tokio::test]
async fn a_compute_offer_publishes_the_whole_shape_of_the_instance_it_prices() {
    let router = router();

    // Rows span the class the rule generalises over: instance family and generation,
    // size (from the smallest burstable one to a size above its family's shared
    // interface and a bare-metal one the normalization table does not rate), processor
    // vendor (Intel, AMD, AWS Graviton), platform, local storage and GPUs. None of them
    // is the recorded t3.nano/t3.micro pair alone.
    let cases: &[ShapeCase<'_>] = &[
        ShapeCase {
            instance_type: "t3.nano",
            operating_system: "Linux",
            storage: "EBS only",
            gpu_memory: "NA",
            processor_features: "AVX; AVX2; Intel AVX; Intel AVX2; Intel AVX512; Intel Turbo",
        },
        ShapeCase {
            instance_type: "t3.micro",
            operating_system: "Windows",
            storage: "EBS only",
            gpu_memory: "NA",
            processor_features: "AVX; AVX2; Intel AVX; Intel AVX2; Intel AVX512; Intel Turbo",
        },
        // Previous generation: EC2-Classic carried it, and its network performance is
        // published as a word tier rather than a bandwidth.
        ShapeCase {
            instance_type: "t2.micro",
            operating_system: "Linux",
            storage: "EBS only",
            gpu_memory: "NA",
            processor_features: "Intel AVX; Intel Turbo",
        },
        ShapeCase {
            instance_type: "m5.24xlarge",
            operating_system: "Linux",
            storage: "EBS only",
            gpu_memory: "NA",
            processor_features: "AVX; AVX2; Intel AVX; Intel AVX2; Intel AVX512; Intel Turbo",
        },
        // A bare-metal size, which the published normalization table does not rate.
        ShapeCase {
            instance_type: "m5.metal",
            operating_system: "Linux",
            storage: "EBS only",
            gpu_memory: "NA",
            processor_features: "AVX; AVX2; Intel AVX; Intel AVX2; Intel AVX512; Intel Turbo",
        },
        // An AMD part carries no Intel feature flag.
        ShapeCase {
            instance_type: "m5a.large",
            operating_system: "Linux",
            storage: "EBS only",
            gpu_memory: "NA",
            processor_features: "AVX; AVX2",
        },
        // A Graviton part: the API publishes no feature list for it.
        ShapeCase {
            instance_type: "m6g.large",
            operating_system: "Linux",
            storage: "EBS only",
            gpu_memory: "NA",
            processor_features: "NA",
        },
        // Storage optimized: an instance store rather than EBS only.
        ShapeCase {
            instance_type: "i3.large",
            operating_system: "Linux",
            storage: "1 x 475 NVMe SSD",
            gpu_memory: "NA",
            processor_features: "Intel AVX; Intel AVX2; Intel Turbo",
        },
        ShapeCase {
            instance_type: "d2.xlarge",
            operating_system: "Linux",
            storage: "3 x 2000 HDD",
            gpu_memory: "NA",
            processor_features: "Intel AVX; Intel AVX2; Intel Turbo",
        },
        // A GPU instance publishes the memory of its GPUs.
        ShapeCase {
            instance_type: "g4dn.xlarge",
            operating_system: "Linux",
            storage: "1 x 125 NVMe SSD",
            gpu_memory: "16 GiB",
            processor_features: "Intel AVX; Intel Turbo",
        },
    ];

    for shape in cases {
        let case = format!("{}/{}", shape.instance_type, shape.operating_system);
        let document = compute_offer(
            &router,
            ComputeSku::in_use(shape.instance_type, shape.operating_system),
        )
        .await;

        // Every published attribute is there, with a value: the identity of the SKU and
        // the whole shape of the instance.
        for name in IDENTITY_ATTRIBUTES.iter().chain(SHAPE_ATTRIBUTES) {
            assert!(
                !attribute_or_empty(&document, name).is_empty(),
                "case {case}: the product must publish {name}, got {document}"
            );
        }

        // What each shape attribute STATES, per the rule it follows.
        assert_eq!(
            attribute_or_empty(&document, "marketoption"),
            "OnDemand",
            "case {case}: these products price on-demand capacity, got {document}"
        );
        assert_eq!(
            attribute_or_empty(&document, "availabilityzone"),
            "NA",
            "case {case}: a Region-scoped product names no availability zone, got {document}"
        );
        assert_eq!(
            attribute_or_empty(&document, "processorArchitecture"),
            "64-bit",
            "case {case}: expected the published architecture, got {document}"
        );
        assert_eq!(
            attribute_or_empty(&document, "vpcnetworkingsupport"),
            "true",
            "case {case}: every priced type launches into a VPC, got {document}"
        );
        // EC2-Classic carried the previous generation of instance types only.
        let current_generation = attribute_or_empty(&document, "currentGeneration");
        assert_eq!(
            attribute_or_empty(&document, "classicnetworkingsupport"),
            if current_generation == "Yes" { "false" } else { "true" },
            "case {case}: classic networking must follow the generation \
             {current_generation:?}, got {document}"
        );
        // The category is the family's name with each word capitalized.
        let family = attribute_or_empty(&document, "instanceFamily");
        assert_eq!(
            attribute_or_empty(&document, "instanceFamilyCategory").to_lowercase(),
            family.to_lowercase(),
            "case {case}: the category must name the family {family:?}, got {document}"
        );
        for name in [
            "enhancedNetworkingSupported",
            "intelAvxAvailable",
            "intelAvx2Available",
            "intelTurboAvailable",
        ] {
            let value = attribute_or_empty(&document, name);
            assert!(
                value == "Yes" || value == "No",
                "case {case}: {name} is published as Yes or No, got {value:?}"
            );
        }
        assert_eq!(
            attribute_or_empty(&document, "storage"),
            shape.storage,
            "case {case}: expected the published instance store, got {document}"
        );
        assert_eq!(
            attribute_or_empty(&document, "gpuMemory"),
            shape.gpu_memory,
            "case {case}: expected the published GPU memory, got {document}"
        );
        assert_eq!(
            attribute_or_empty(&document, "processorFeatures"),
            shape.processor_features,
            "case {case}: expected the features published for {:?}, got {document}",
            attribute_or_empty(&document, "physicalProcessor")
        );
        // A bandwidth attribute states a figure, or NA where the type has no dedicated
        // attachment of that kind.
        for name in ["dedicatedEbsThroughput", "dedicatedEbsThroughputDescription"] {
            let value = attribute_or_empty(&document, name);
            assert!(
                value == "NA" || (value.ends_with(" Mbps") && leading_figure(&value).is_some()),
                "case {case}: {name} states an Mbps figure or NA, got {value:?}"
            );
        }
        let network = attribute_or_empty(&document, "networkPerformance");
        assert!(
            network.ends_with("Gigabit") || !network.contains(char::is_numeric),
            "case {case}: networkPerformance states a bandwidth or a word tier, got \
             {network:?}"
        );

        // The shape belongs to the instance TYPE, so every SKU of that type publishes the
        // same one however its hour is bought.
        let other = compute_offer(
            &router,
            ComputeSku {
                instance_type: shape.instance_type,
                operating_system: shape.operating_system,
                tenancy: "Dedicated",
                capacity_status: "UnusedCapacityReservation",
            },
        )
        .await;
        for name in SHAPE_ATTRIBUTES {
            assert_eq!(
                attribute_or_empty(&document, name),
                attribute_or_empty(&other, name),
                "case {case}: {name} is a property of the instance type, so every SKU of \
                 it must publish the same value"
            );
        }
    }
}

#[tokio::test]
async fn the_shared_bandwidth_of_a_family_grows_with_the_size_it_is_shared_by() {
    let router = router();

    // One family, four sizes: the attachment's ceiling is the family's and does not move
    // with the size, while the baseline the description states is the size's own share of
    // it and grows with the size.
    let sizes = ["t3.nano", "t3.micro", "t3.small", "t3.medium"];
    let mut ceilings: BTreeSet<String> = BTreeSet::new();
    let mut baselines: Vec<u64> = Vec::new();
    for instance_type in sizes {
        let document = compute_offer(&router, ComputeSku::in_use(instance_type, "Linux")).await;
        let ceiling = attribute_or_empty(&document, "dedicatedEbsThroughput");
        assert!(
            ceiling.starts_with("Up to "),
            "case {instance_type}: a size that shares its family's attachment states an \
             'Up to' ceiling, got {ceiling:?}"
        );
        ceilings.insert(ceiling.clone());
        let baseline = leading_figure(&attribute_or_empty(
            &document,
            "dedicatedEbsThroughputDescription",
        ))
        .unwrap_or_else(|| {
            panic!("case {instance_type}: the description must state a baseline figure")
        });
        let ceiling_figure = leading_figure(&ceiling).unwrap_or_default();
        assert!(
            baseline > 0 && baseline <= ceiling_figure,
            "case {instance_type}: the baseline {baseline} must be positive and within the \
             ceiling {ceiling_figure}"
        );
        baselines.push(baseline);
    }
    assert_eq!(
        ceilings.len(),
        1,
        "the ceiling belongs to the family, so every size must state the same one, got \
         {ceilings:?}"
    );
    for pair in baselines.windows(2) {
        assert!(
            pair[0] < pair[1],
            "a larger size must sustain more of the shared attachment, got {baselines:?} \
             for {sizes:?}"
        );
    }
}

#[tokio::test]
async fn a_shape_attribute_selects_the_products_that_publish_it() {
    let router = router();

    // Each row filters on a shape attribute the document now publishes — the lookups a
    // right-sizing client makes — paired with an attribute that narrows the answer.
    let cases: &[(&str, &str, &str)] = &[
        ("marketoption", "OnDemand", "t3.nano"),
        ("availabilityzone", "NA", "t3.micro"),
        ("storage", "EBS only", "m5.large"),
        ("storage", "1 x 475 NVMe SSD", "i3.large"),
        ("networkPerformance", "Up to 5 Gigabit", "t3.small"),
        ("processorArchitecture", "64-bit", "m6g.large"),
        ("intelAvx2Available", "No", "m5a.large"),
        ("clockSpeed", "3.1 GHz", "m5.large"),
        ("instanceFamilyCategory", "Storage Optimized", "i3.large"),
        ("gpuMemory", "16 GiB", "g4dn.xlarge"),
    ];

    for (field, value, instance_type) in cases.iter().copied() {
        let case = format!("{field}={value} {instance_type}");
        let response = ok(
            &router,
            "GetProducts",
            &json!({
                "ServiceCode": "AmazonEC2",
                "Filters": term_matches(&[
                    (field, value),
                    ("instanceType", instance_type),
                    ("regionCode", "us-east-1"),
                    ("tenancy", "Shared"),
                    ("capacitystatus", "Used"),
                ]),
                "MaxResults": 100,
            }),
        )
        .await;
        let documents = offer_documents(&response);
        assert!(
            !documents.is_empty(),
            "case {case}: a filter on a published attribute value must select the products \
             that carry it, got {response}"
        );
        for document in &documents {
            assert_eq!(
                attribute_or_empty(document, field).to_lowercase(),
                value.to_lowercase(),
                "case {case}: every selected product must carry that value, got {document}"
            );
        }
    }
}

#[tokio::test]
async fn a_reserved_compute_term_describes_the_usage_the_reservation_applies_to() {
    let router = router();

    // Rows differ in platform (the reserved platform name the API writes for it),
    // instance family and size.
    let cases: &[(&str, &str, &str)] = &[
        ("t3.nano", "Linux", "Linux/UNIX (Amazon VPC)"),
        ("t3.micro", "Linux", "Linux/UNIX (Amazon VPC)"),
        ("m5.large", "Windows", "Windows (Amazon VPC)"),
        ("t3.micro", "Windows", "Windows (Amazon VPC)"),
        ("r5.large", "Linux", "Linux/UNIX (Amazon VPC)"),
        ("m6g.2xlarge", "Linux", "Linux/UNIX (Amazon VPC)"),
    ];

    for (instance_type, operating_system, platform) in cases.iter().copied() {
        let case = format!("{instance_type}/{operating_system}");
        let document = compute_offer(
            &router,
            ComputeSku::in_use(instance_type, operating_system),
        )
        .await;

        // The reserved usage dimension names the platform and the instance type the
        // reservation applies to, and states no rate: the rate is its own pricePerUnit.
        let expected = format!("{platform}, {instance_type} reserved instance applied");
        let reserved = terms_of(&document, "Reserved");
        assert!(
            !reserved.is_empty(),
            "case {case}: a compute instance is also offered under a reservation, got \
             {document}"
        );
        for (code, term) in &reserved {
            for (rate_code, dimension) in dimensions_of(term) {
                let unit = attribute_text(&dimension, "unit");
                let description = attribute_text(&dimension, "description");
                let rate = dimension_rate(&dimension);
                if unit == "Hrs" {
                    assert_eq!(
                        description, expected,
                        "case {case}/{code}: expected the platform-and-type sentence, got \
                         {description:?}"
                    );
                    assert!(
                        !description.contains(&display_rate(&rate))
                            && !description.contains("USD"),
                        "case {case}/{code}: the reserved usage sentence must not restate \
                         the rate {rate}, got {description:?}"
                    );
                } else {
                    // The upfront fee's wording is unchanged by the rule above.
                    assert_eq!(
                        unit, "Quantity",
                        "case {case}/{rate_code}: a one-off fee is a quantity, got {dimension}"
                    );
                    assert_eq!(
                        description, "Upfront Fee",
                        "case {case}/{rate_code}: expected the upfront-fee description, got \
                         {description:?}"
                    );
                }
            }
        }

        // The sibling on-demand dimension DOES state its rate, and keeps doing so.
        let on_demand = terms_of(&document, "OnDemand");
        for (code, term) in &on_demand {
            for (_, dimension) in dimensions_of(term) {
                let rate = dimension_rate(&dimension);
                assert_eq!(
                    attribute_text(&dimension, "description"),
                    format!(
                        "${} per On Demand {operating_system} {instance_type} Instance Hour",
                        display_rate(&rate)
                    ),
                    "case {case}/{code}: the on-demand sentence states the rate and the \
                     instance it prices, got {dimension}"
                );
            }
        }
    }
}

/// One string member of a price dimension, or an empty string when it carries none.
fn attribute_text(dimension: &Value, member: &str) -> String {
    dimension
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The start of the month the clock is in, written as the Price List API dates a
/// publication.
fn current_month_start() -> String {
    let now = ::aws_smithy_types::DateTime::from(std::time::SystemTime::now())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the current instant is representable");
    let month = now
        .get(..7)
        .expect("an RFC 3339 instant starts with its year and month");
    format!("{month}-01T00:00:00Z")
}

/// The digits of a timestamp, which is the stamp a publication is versioned by.
fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

#[tokio::test]
async fn an_offer_document_is_dated_at_the_catalogs_current_publication() {
    let router = router();

    // Rows differ in service and product family, and in whether the product is also
    // leased under a reservation: the publication dates the whole catalog, not one
    // family.
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "AmazonEC2",
            &[("instanceType", "t3.nano"), ("operatingSystem", "Linux")],
        ),
        (
            "AmazonEC2",
            &[("instanceType", "m5.large"), ("operatingSystem", "Windows")],
        ),
        ("AmazonEC2", &[("volumeApiName", "gp3")]),
        ("AmazonS3", &[("storageClass", "Archive")]),
        ("AWSLambda", &[("regionCode", "us-east-1")]),
    ];

    let published = current_month_start();
    for (service_code, filters) in cases.iter().copied() {
        let case = format!("{service_code} {filters:?}");
        let response = ok(
            &router,
            "GetProducts",
            &json!({
                "ServiceCode": service_code,
                "Filters": term_matches(filters),
                "MaxResults": 100,
            }),
        )
        .await;
        let documents = offer_documents(&response);
        assert!(
            !documents.is_empty(),
            "case {case}: expected at least one priced product, got {response}"
        );
        for document in &documents {
            let sku = product_sku(document);
            let publication = document
                .get("publicationDate")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // The publication is the start of the month in effect when it is served, not
            // a date fixed when the catalog was written.
            assert_eq!(
                publication, published,
                "case {case}: {sku} must be dated at the current publication"
            );
            // The version stamp and every term's effective date name that same
            // publication, so a client keying a cache on any of them sees one publication.
            assert_eq!(
                document.get("version").and_then(Value::as_str),
                Some(digits(&publication).as_str()),
                "case {case}: {sku}'s version is the publication instant's stamp, got \
                 {document}"
            );
            let term_types: Vec<String> = document
                .get("terms")
                .and_then(Value::as_object)
                .map(|terms| terms.keys().cloned().collect())
                .unwrap_or_default();
            assert!(
                !term_types.is_empty(),
                "case {case}: {sku} must be priced under at least one term type"
            );
            for term_type in &term_types {
                for (code, term) in terms_of(document, term_type) {
                    assert_eq!(
                        term.get("effectiveDate").and_then(Value::as_str),
                        Some(publication.as_str()),
                        "case {case}: {term_type} term {code} must take effect from the \
                         publication it is served under, got {term}"
                    );
                }
            }
        }
    }
}
