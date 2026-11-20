// SPDX-License-Identifier: CC0-1.0

//! What ONE ROW of a purchased plan's rate card costs: the rate of THAT row's
//! own dimension tuple, under THAT plan.
//!
//! A rate card is a price lookup, so every row has to be priced from the
//! catalog entry it projects rather than from one figure reused across the
//! card. The rules asserted here, each over a table of plans and of priced
//! combinations:
//!
//! * a row's rate follows the row's own dimensions: within one instance type,
//!   the card holds exactly as many distinct rates as the platforms it prices
//!   hold distinct licence costs, and a costlier licence prices strictly higher
//!   — so no two platforms whose licences differ can share a rate, and a card
//!   that reported one figure for every platform would fail here;
//! * the same platform on a bigger instance of one family prices strictly
//!   higher, in one order that holds for every platform;
//! * the same row under a differently discounted commitment prices differently:
//!   a commitment scoped to one instance family in one region prices the usage
//!   it covers strictly below an unscoped one of the same term and payment
//!   option;
//! * a row's `operation` is the usage-operation code the catalog pairs with the
//!   row's own `productDescription` ([`state::EC2_PLATFORMS`], AWS's published
//!   platform-details-to-usage-operation table), each code unique within the
//!   (region, instance type, tenancy) group it prices, and spelled in the
//!   published form: the bare operation name, or that name and `:` followed by
//!   the four-character suffix AWS assigns the platform, whose characters are
//!   drawn from the digits and `a`-`g` that table uses (`RunInstances:00g0` for
//!   Red Hat BYOL Linux, `RunInstances:000g` for SUSE Linux) — so a suffix
//!   computed from a row's position could not pass.
//!
//! The suite derives the region, the instance family and the platforms it drives
//! from the service's own catalog rather than naming any recorded value, and
//! everything goes through the same `MultiServiceRouter` the emulator boots,
//! reusing the harness in [`crate::savings_plans_lifecycle`].

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_savingsplans_awssavingsplan::state;

use crate::savings_plans_lifecycle::{text, Emulator, ONE_YEAR};

/// The operation under test.
const OPERATION: &str = "DescribeSavingsPlanRates";

/// The largest page the search admits (`MaxResults` tops out at 1000).
const LARGEST_PAGE: i64 = 1000;

/// The characters a usage-operation suffix is spelled with, from the
/// platform-details table the catalog carries ([`state::EC2_PLATFORMS`]): the
/// decimal digits and `a`-`g`.
const SUFFIX_CHARACTERS: &str = "0123456789abcdefg";

/// How long a usage-operation suffix is in that table.
const SUFFIX_LENGTH: usize = 4;

/// A plan's rate card as this suite reads it: the rate of each combination it
/// prices, keyed by `(instanceType, productDescription)`.
type PricedCombinations = BTreeMap<(String, String), i128>;

/// The dimensions a rate is priced along, as `name -> value`.
fn dimensions(rate: &Value) -> BTreeMap<String, String> {
    rate.get("properties")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|property| (text(property, "name"), text(property, "value")))
                .collect()
        })
        .unwrap_or_default()
}

/// The value of one dimension of a rate, or `""`.
fn dimension(rate: &Value, name: &str) -> String {
    dimensions(rate).get(name).cloned().unwrap_or_default()
}

/// A rate as a number, in the units the service holds amounts at.
fn units(case: &str, rate: &Value) -> i128 {
    let rendered = text(rate, "rate");
    state::parse_amount_at_wire_scale(&rendered)
        .unwrap_or_else(|| panic!("case {case:?}: expected a decimal rate, got {rendered:?}"))
}

/// The value of an offering property (`SavingsPlanOfferingPropertyKey`), or `""`
/// when the offering is not scoped to it.
fn offering_property(offering: &Value, name: &str) -> String {
    offering
        .get("properties")
        .and_then(Value::as_array)
        .and_then(|list| list.iter().find(|entry| text(entry, "name") == name))
        .map(|entry| text(entry, "value"))
        .unwrap_or_default()
}

/// Every rate of one plan a request admits, walked through the search's own
/// cursor.
async fn card(emulator: &Emulator, case: &str, request: &Value) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut page = request.clone();
        page["maxResults"] = json!(LARGEST_PAGE);
        if let Some(token) = &next {
            page["nextToken"] = json!(token);
        }
        let answer = emulator.call(OPERATION, page.clone()).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 for {page}, got {} {:?}",
            answer.status, answer.body
        );
        rows.extend(answer.list("searchResults").iter().cloned());
        next = answer.cursor();
        assert!(
            rows.len() <= 10_000,
            "case {case:?}: a walk must terminate, got {} rows",
            rows.len()
        );
        if next.is_none() {
            return rows;
        }
    }
}

/// The rate card of one plan, keyed by the combination each row prices:
/// `(instanceType, productDescription) -> rate`.
async fn rates_by_combination(
    emulator: &Emulator,
    case: &str,
    savings_plan_id: &str,
    region: &str,
    instance_family: &str,
) -> PricedCombinations {
    // `SavingsPlanRateFilterName` names the dimensions of the USAGE priced —
    // the instance type among them, the family not — so the card is narrowed to
    // the region and to EC2 usage on the wire, and to the family under test
    // here.
    let rows = card(
        emulator,
        case,
        &json!({
            "savingsPlanId": savings_plan_id,
            "filters": [
                { "name": "region", "values": [region] },
                { "name": "productType", "values": ["EC2"] },
            ],
        }),
    )
    .await;
    assert!(
        !rows.is_empty(),
        "case {case:?}: the commitment covers {instance_family:?} usage in {region:?}, so its \
         card must price it"
    );

    let prefix = format!("{instance_family}.");
    let mut priced: PricedCombinations = BTreeMap::new();
    for rate in &rows {
        let instance_type = dimension(rate, state::RATE_PROPERTY_INSTANCE_TYPE);
        if !instance_type.starts_with(&prefix) {
            continue;
        }
        let product_description = dimension(rate, state::RATE_PROPERTY_PRODUCT_DESCRIPTION);
        let row = format!("{case} / {instance_type} / {product_description}");
        // Every row of an EC2 card names the usage it prices, and names it once.
        assert!(
            !instance_type.is_empty() && !product_description.is_empty(),
            "case {row:?}: an EC2 rate names the instance type and the platform it prices, got \
             {:?}",
            dimensions(rate)
        );
        assert_eq!(
            dimension(rate, state::PROPERTY_REGION),
            region,
            "case {row:?}: the region filter admits only {region:?}, got {:?}",
            dimensions(rate)
        );
        assert!(
            priced
                .insert((instance_type, product_description), units(&row, rate))
                .is_none(),
            "case {row:?}: one combination is priced once, got it twice"
        );
    }
    priced
}

/// Buy one offering, asserting the purchase succeeded.
async fn buy(emulator: &Emulator, offering: &Value, commitment: &str) -> String {
    emulator
        .purchase(json!({
            "savingsPlanOfferingId": text(offering, "offeringId"),
            "commitment": commitment,
        }))
        .await
}

/// Within one instance type, a row's rate follows that row's licence cost; and
/// the same platform prices strictly higher on a bigger instance of the family.
#[tokio::test]
async fn a_rate_card_prices_every_combination_from_its_own_catalog_entry() {
    let emulator = Emulator::new();

    // The region and instance family an EC2 Instance offering is sold for: the
    // scope both commitments below cover, taken from the catalog rather than
    // named here.
    let scoped = emulator
        .offering("EC2Instance", state::PAYMENT_NO_UPFRONT, ONE_YEAR)
        .await;
    let region = offering_property(&scoped, state::PROPERTY_REGION);
    let family = offering_property(&scoped, state::PROPERTY_INSTANCE_FAMILY);
    assert!(
        !region.is_empty() && !family.is_empty(),
        "an EC2 Instance offering names the scope it is sold for, got {scoped:?}"
    );

    let unscoped = emulator
        .offering("Compute", state::PAYMENT_NO_UPFRONT, ONE_YEAR)
        .await;

    // (case, the offering bought) — the rows differ in the SCOPE of the
    // commitment, which is what its discount follows, so the same combination
    // is priced by two different commitments.
    let mut cards: Vec<(&str, PricedCombinations)> = Vec::new();
    for (case, offering) in [
        ("a commitment scoped to one instance family", &scoped),
        ("a commitment scoped to nothing at all", &unscoped),
    ] {
        let savings_plan_id = buy(&emulator, offering, "0.5").await;
        let priced =
            rates_by_combination(&emulator, case, &savings_plan_id, &region, &family).await;

        // Every platform of the catalog is priced for every instance type the
        // card holds, and each row carries that platform's own code.
        let platforms: BTreeSet<String> = state::EC2_PLATFORMS
            .iter()
            .map(|(description, _, _)| (*description).to_string())
            .collect();
        let instance_types: BTreeSet<String> =
            priced.keys().map(|(instance, _)| instance.clone()).collect();
        assert!(
            instance_types.len() > 1,
            "case {case:?}: the family is sold in more than one size, got {instance_types:?}"
        );
        for instance_type in &instance_types {
            let in_type: BTreeMap<String, i128> = priced
                .iter()
                .filter(|((instance, _), _)| instance == instance_type)
                .map(|((_, description), rate)| (description.clone(), *rate))
                .collect();
            assert_eq!(
                in_type.keys().cloned().collect::<BTreeSet<String>>(),
                platforms,
                "case {case:?} / {instance_type}: the card prices exactly the platforms the \
                 catalog carries"
            );

            // The rule: a row's rate is the rate of that row's dimensions, so
            // the card holds one distinct rate per distinct licence cost — not
            // one rate for the whole card, and not one rate for two platforms
            // whose licences differ.
            let licences: BTreeSet<i128> = state::EC2_PLATFORMS
                .iter()
                .map(|(_, _, licence)| *licence)
                .collect();
            let distinct: BTreeSet<i128> = in_type.values().copied().collect();
            assert_eq!(
                distinct.len(),
                licences.len(),
                "case {case:?} / {instance_type}: expected one rate per distinct licence cost \
                 ({} of them), got {} distinct rates in {in_type:?}",
                licences.len(),
                distinct.len()
            );

            // And it is ORDERED by that licence cost, over every pair the
            // catalog carries.
            for (description, _, licence) in state::EC2_PLATFORMS {
                for (other, _, other_licence) in state::EC2_PLATFORMS {
                    let (Some(rate), Some(other_rate)) =
                        (in_type.get(description), in_type.get(other))
                    else {
                        panic!("case {case:?} / {instance_type}: both platforms are priced");
                    };
                    let row = format!("{case} / {instance_type} / {description} vs {other}");
                    if licence > other_licence {
                        assert!(
                            rate > other_rate,
                            "case {row:?}: a costlier licence must price strictly higher, got \
                             {rate} and {other_rate}"
                        );
                    } else if licence == other_licence {
                        assert_eq!(
                            rate, other_rate,
                            "case {row:?}: platforms whose licences cost the same must price the \
                             same, got {rate} and {other_rate}"
                        );
                    }
                }
            }
        }

        // One size order holds for every platform: a bigger instance of the
        // family costs strictly more for the same usage. The order is read off
        // the unlicensed platform and then required of all the others.
        let unlicensed = state::EC2_PLATFORMS[0].0;
        let mut by_size: Vec<(String, i128)> = instance_types
            .iter()
            .map(|instance_type| {
                let key = (instance_type.clone(), unlicensed.to_string());
                let rate = *priced.get(&key).unwrap_or_else(|| {
                    panic!("case {case:?}: {unlicensed:?} is priced for {instance_type}")
                });
                (instance_type.clone(), rate)
            })
            .collect();
        by_size.sort_by_key(|(_, rate)| *rate);
        for window in by_size.windows(2) {
            let (smaller, smaller_rate) = &window[0];
            let (bigger, bigger_rate) = &window[1];
            assert!(
                smaller_rate < bigger_rate,
                "case {case:?}: two sizes of one family must not price the same, got {smaller} at \
                 {smaller_rate} and {bigger} at {bigger_rate}"
            );
            for (description, _, _) in state::EC2_PLATFORMS {
                let row = format!("{case} / {description} / {smaller} vs {bigger}");
                let small = priced
                    .get(&(smaller.clone(), description.to_string()))
                    .copied()
                    .unwrap_or_default();
                let big = priced
                    .get(&(bigger.clone(), description.to_string()))
                    .copied()
                    .unwrap_or_default();
                assert!(
                    small < big,
                    "case {row:?}: the bigger instance must price strictly higher for every \
                     platform, got {small} and {big}"
                );
            }
        }

        cards.push((case, priced));
    }

    // The same combination under two commitments: the one scoped to a single
    // instance family in a single region earns the deeper discount, so it
    // prices every row it covers strictly below the unscoped commitment.
    let (scoped_case, scoped_card) = &cards[0];
    let (unscoped_case, unscoped_card) = &cards[1];
    assert_eq!(
        scoped_card.keys().collect::<Vec<_>>(),
        unscoped_card.keys().collect::<Vec<_>>(),
        "both commitments cover the same combinations"
    );
    for (combination, scoped_rate) in scoped_card {
        let unscoped_rate = unscoped_card.get(combination).copied().unwrap_or_default();
        assert!(
            *scoped_rate < unscoped_rate,
            "case {combination:?}: {scoped_case:?} must price below {unscoped_case:?}, got \
             {scoped_rate} and {unscoped_rate}"
        );
    }
}

/// Every row's `operation` is the code the catalog pairs with that row's
/// `productDescription`, unique within the group it prices, and spelled in the
/// published form.
#[tokio::test]
async fn a_rate_names_the_usage_operation_code_of_the_platform_it_prices() {
    let emulator = Emulator::new();
    let scoped = emulator
        .offering("EC2Instance", state::PAYMENT_ALL_UPFRONT, ONE_YEAR)
        .await;
    let region = offering_property(&scoped, state::PROPERTY_REGION);
    let family = offering_property(&scoped, state::PROPERTY_INSTANCE_FAMILY);
    let savings_plan_id = buy(&emulator, &scoped, "1").await;

    let rows = card(
        &emulator,
        "the card of a commitment scoped to one instance family",
        &json!({
            "savingsPlanId": savings_plan_id,
            "filters": [
                { "name": "region", "values": [region] },
                { "name": "instanceType", "values": [format!("{family}.large")] },
            ],
        }),
    )
    .await;
    assert!(!rows.is_empty(), "the commitment prices the usage it covers");

    // The catalog's own pairing, which every row must agree with.
    let published: BTreeMap<String, String> = state::EC2_PLATFORMS
        .iter()
        .map(|(description, operation, _)| ((*description).to_string(), (*operation).to_string()))
        .collect();
    // (region, instanceType, tenancy) -> the codes seen in that group.
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for rate in &rows {
        let description = dimension(rate, state::RATE_PROPERTY_PRODUCT_DESCRIPTION);
        let instance_type = dimension(rate, state::RATE_PROPERTY_INSTANCE_TYPE);
        let tenancy = dimension(rate, state::RATE_PROPERTY_TENANCY);
        let operation = text(rate, "operation");
        let row = format!("{instance_type} / {tenancy} / {description}");

        assert_eq!(
            published.get(&description).cloned().unwrap_or_default(),
            operation,
            "case {row:?}: a row's code must be the one the catalog pairs with its platform, got \
             {operation:?}"
        );

        // The published FORM: the bare operation name, or that name, ':' and a
        // four-character suffix drawn from the table's own alphabet. A suffix
        // derived from a row's position rather than taken from the table would
        // fail this.
        let (name, suffix) = match operation.split_once(':') {
            Some((name, suffix)) => (name, Some(suffix)),
            None => (operation.as_str(), None),
        };
        assert!(
            !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric()),
            "case {row:?}: expected a usage-operation name, got {operation:?}"
        );
        if let Some(suffix) = suffix {
            assert_eq!(
                suffix.chars().count(),
                SUFFIX_LENGTH,
                "case {row:?}: a published suffix is {SUFFIX_LENGTH} characters, got {suffix:?}"
            );
            assert!(
                suffix.chars().all(|ch| SUFFIX_CHARACTERS.contains(ch)),
                "case {row:?}: a suffix is spelled with {SUFFIX_CHARACTERS:?}, got {suffix:?}"
            );
        }

        let group = format!("{region}|{instance_type}|{tenancy}");
        assert!(
            seen.entry(group.clone()).or_default().insert(operation.clone()),
            "case {row:?}: {operation:?} already priced another platform of {group}"
        );
    }

    // Every platform the catalog carries was exercised, including those no
    // single recorded card happens to hold.
    let exercised: BTreeSet<String> = rows
        .iter()
        .map(|rate| dimension(rate, state::RATE_PROPERTY_PRODUCT_DESCRIPTION))
        .collect();
    assert_eq!(
        exercised,
        published.keys().cloned().collect::<BTreeSet<String>>(),
        "the card prices every platform the catalog carries"
    );
}
