// SPDX-License-Identifier: CC0-1.0

//! What a plan's EC2 rate card holds: one rate per PLATFORM of each instance
//! type the commitment covers.
//!
//! EC2 bills usage per platform — the operating system and database licence the
//! instance runs — and names which one through the usage-operation code of the
//! line item, so the rules asserted here are:
//!
//! * a commitment that covers EC2 is priced for every platform of each instance
//!   type it covers, one rate each, with that platform's usage-operation code as
//!   the rate's `operation`;
//! * a rate's `usageType` names the usage alone (`BoxUsage:<instanceType>`): the
//!   region it is billed in is one of the rate's dimensions, not a prefix on the
//!   usage type, so the usage type joins against a usage record's own;
//! * a plan rate reports the dimensions of the usage it prices — region,
//!   instance type, platform and tenancy — and not the instance FAMILY, which is
//!   a dimension of the commitment's scope and is implied by the instance type;
//! * a licensed platform prices above the same instance type's unlicensed Linux
//!   rate, which is the ordering AWS publishes for licence pricing.
//!
//! The platform catalog under test is [`state::EC2_PLATFORMS`], whose
//! `productDescription`/`operation` pairs are AWS's published platform-details
//! to usage-operation table; every row of it is exercised, including platforms no
//! recorded card of this suite's writing happened to carry. Everything goes
//! through the same `MultiServiceRouter` the emulator boots, reusing the harness
//! in [`crate::savings_plans_lifecycle`].

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_savingsplans_awssavingsplan::state;

use crate::savings_plans_lifecycle::{text, Emulator, ONE_YEAR, THREE_YEARS};

/// The operation under test.
const OPERATION: &str = "DescribeSavingsPlanRates";

/// The largest page the search admits (`MaxResults` tops out at 1000).
const LARGEST_PAGE: i64 = 1000;

/// The dimensions a rate is priced along, as `name -> value`.
fn properties(rate: &Value) -> BTreeMap<String, String> {
    rate.get("properties")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|property| (text(property, "name"), text(property, "value")))
                .collect()
        })
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
            rows.len() <= 100_000,
            "case {case:?}: a walk must terminate, got {} rows",
            rows.len()
        );
        if next.is_none() {
            return rows;
        }
    }
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

/// Buy `offering`, asserting the purchase succeeded.
async fn buy_offering(emulator: &Emulator, offering: &Value, commitment: &str) -> String {
    emulator
        .purchase(json!({
            "savingsPlanOfferingId": text(offering, "offeringId"),
            "commitment": commitment,
        }))
        .await
}

/// Buy one offering of this plan type, payment option and term.
async fn buy(
    emulator: &Emulator,
    plan_type: &str,
    payment_option: &str,
    duration: i64,
    commitment: &str,
) -> String {
    let offering = emulator.offering(plan_type, payment_option, duration).await;
    buy_offering(emulator, &offering, commitment).await
}

/// A rate card holds one rate per platform of the instance type it prices, each
/// carrying that platform's usage-operation code, an unprefixed usage type and
/// exactly the dimensions of the usage it prices.
///
/// The rows are the plan types that cover EC2 usage — one scoped to a single
/// instance family in one region and one scoped to neither — narrowed to one
/// instance type in one region, so the set under test is exactly the platforms.
#[tokio::test]
async fn an_ec2_rate_card_prices_every_platform_of_an_instance_type() {
    let emulator = Emulator::new();

    for (case, plan_type, payment_option, duration, commitment) in [
        (
            "a commitment scoped to one instance family",
            "EC2Instance",
            state::PAYMENT_NO_UPFRONT,
            ONE_YEAR,
            "0.5",
        ),
        (
            "a commitment scoped to no family at all",
            "Compute",
            state::PAYMENT_ALL_UPFRONT,
            THREE_YEARS,
            "1.5",
        ),
    ] {
        // The region and instance family an EC2 Instance offering of the same
        // term is sold for: the instance type under test, so one request admits
        // one instance type in one region under either plan type.
        let scoped = emulator
            .offering("EC2Instance", payment_option, duration)
            .await;
        let region = offering_property(&scoped, state::PROPERTY_REGION);
        let family = offering_property(&scoped, state::PROPERTY_INSTANCE_FAMILY);
        assert!(
            !region.is_empty() && !family.is_empty(),
            "case {case:?}: an EC2 Instance offering names the scope it is sold for, got {scoped:?}"
        );
        let instance_type = format!("{family}.large");
        let savings_plan_id = if plan_type == "EC2Instance" {
            buy_offering(&emulator, &scoped, commitment).await
        } else {
            buy(&emulator, plan_type, payment_option, duration, commitment).await
        };

        let rows = card(
            &emulator,
            case,
            &json!({
                "savingsPlanId": savings_plan_id,
                "filters": [
                    { "name": "region", "values": [region] },
                    { "name": "instanceType", "values": [instance_type] },
                ],
            }),
        )
        .await;

        // One row per platform, keyed by the platform's own usage-operation
        // code, and nothing else.
        let by_operation: BTreeMap<String, Value> = rows
            .iter()
            .map(|rate| (text(rate, "operation"), rate.clone()))
            .collect();
        assert_eq!(
            by_operation.len(),
            rows.len(),
            "case {case:?}: each platform is priced once, got {} rows for {} operation codes",
            rows.len(),
            by_operation.len()
        );
        assert_eq!(
            by_operation.keys().cloned().collect::<BTreeSet<String>>(),
            state::EC2_PLATFORMS
                .iter()
                .map(|(_, operation, _)| (*operation).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case:?}: the card must price exactly the platforms the catalog carries"
        );

        let linux = state::EC2_PLATFORMS[0];
        for (product_description, operation, licence_units) in state::EC2_PLATFORMS {
            let row = format!("{case} / {product_description}");
            let rate = by_operation.get(operation).unwrap_or_else(|| {
                panic!("case {row:?}: expected a rate under operation {operation:?}")
            });
            // The usage type names the usage alone: no region prefix.
            assert_eq!(
                text(rate, "usageType"),
                format!("BoxUsage:{instance_type}"),
                "case {row:?}: expected the unprefixed usage type of {instance_type:?}, got {:?}",
                text(rate, "usageType")
            );
            // Exactly the dimensions of the usage priced, the platform among
            // them and the instance family not.
            assert_eq!(
                properties(rate),
                BTreeMap::from([
                    (state::PROPERTY_REGION.to_string(), region.clone()),
                    (
                        state::RATE_PROPERTY_INSTANCE_TYPE.to_string(),
                        instance_type.clone()
                    ),
                    (
                        state::RATE_PROPERTY_PRODUCT_DESCRIPTION.to_string(),
                        product_description.to_string()
                    ),
                    (
                        state::RATE_PROPERTY_TENANCY.to_string(),
                        state::TENANCY_SHARED.to_string()
                    ),
                ]),
                "case {row:?}: expected the dimensions of the usage priced, got {:?}",
                properties(rate)
            );
            assert_eq!(
                text(rate, "productType"),
                "EC2",
                "case {row:?}: an EC2 line item is EC2 usage, got {:?}",
                text(rate, "productType")
            );
            assert_eq!(
                text(rate, "serviceCode"),
                "AmazonEC2",
                "case {row:?}: EC2 bills this usage, got {:?}",
                text(rate, "serviceCode")
            );

            // A licence costs money: a platform carrying one prices above the
            // unlicensed Linux rate of the same instance type, and a platform
            // that carries none prices exactly as it does.
            let units = |rate: &Value| {
                let rendered = text(rate, "rate");
                state::parse_amount_at_wire_scale(&rendered).unwrap_or_else(|| {
                    panic!("case {row:?}: {rendered:?} must be a decimal amount")
                })
            };
            let unlicensed = by_operation
                .get(linux.1)
                .map(units)
                .unwrap_or_else(|| panic!("case {row:?}: the card prices unlicensed Linux"));
            let priced = units(rate);
            if licence_units > linux.2 {
                assert!(
                    priced > unlicensed,
                    "case {row:?}: a licensed platform must price above unlicensed Linux \
                     ({unlicensed} units), got {priced} units"
                );
            } else {
                assert_eq!(
                    priced, unlicensed,
                    "case {row:?}: a platform that carries no licence must price as unlicensed \
                     Linux ({unlicensed} units), got {priced} units"
                );
            }
        }
    }
}

/// A rate card is filtered by the platform the caller names, so a client pricing
/// one platform's usage reads exactly that platform's rate.
///
/// The rows are platforms the catalog carries that are neither the unlicensed
/// Linux row nor the Windows one every example names: each must be selectable by
/// `productDescription` and by its own usage-operation code.
#[tokio::test]
async fn a_rate_card_is_filtered_by_the_platform_named() {
    let emulator = Emulator::new();
    let savings_plan_id = buy(
        &emulator,
        "Compute",
        state::PAYMENT_NO_UPFRONT,
        ONE_YEAR,
        "1",
    )
    .await;

    for product_description in [
        "SUSE Linux",
        "Ubuntu Pro",
        "Red Hat Enterprise Linux with HA",
        "Linux with SQL Server Web",
    ] {
        let expected_operation = state::EC2_PLATFORMS
            .iter()
            .find(|(description, _, _)| *description == product_description)
            .map(|(_, operation, _)| *operation)
            .unwrap_or_else(|| panic!("case {product_description:?}: the catalog carries it"));

        for (case, filter) in [
            (
                format!("{product_description} by productDescription"),
                json!({ "name": "productDescription", "values": [product_description] }),
            ),
            (
                format!("{product_description} by its usage-operation code"),
                json!({ "name": "operation", "values": [expected_operation] }),
            ),
        ] {
            let rows = card(
                &emulator,
                &case,
                &json!({ "savingsPlanId": savings_plan_id, "filters": [filter] }),
            )
            .await;
            assert!(
                !rows.is_empty(),
                "case {case:?}: the catalog prices this platform, so the search must answer with it"
            );
            for rate in &rows {
                assert_eq!(
                    properties(rate)
                        .get(state::RATE_PROPERTY_PRODUCT_DESCRIPTION)
                        .cloned()
                        .unwrap_or_default(),
                    product_description,
                    "case {case:?}: every admitted rate must price {product_description:?}, got {:?}",
                    properties(rate)
                );
                assert_eq!(
                    text(rate, "operation"),
                    expected_operation,
                    "case {case:?}: {product_description:?} is billed under {expected_operation:?}, got {:?}",
                    text(rate, "operation")
                );
            }
        }
    }
}
