// SPDX-License-Identifier: CC0-1.0

//! What a rate card REPORTS for a row: the catalog's price for that row's own
//! dimensions, spelled as the catalog holds it, in the catalog's order.
//!
//! A `SavingsPlanRate` is a line of a price list, so the rules asserted here
//! are:
//!
//! * a row's `rate` is the catalog's price for THAT row's dimension tuple — the
//!   on-demand price of its own instance type and platform less the discount the
//!   commitment earns — so two rows of one instance type that differ in platform
//!   differ in price;
//! * the price is held and reported at the precision the catalog quotes that
//!   product's prices at, never padded out to the billing scale
//!   ([`state::AMOUNT_WIRE_DIGITS`]) a plan's commitment and payments are held
//!   at: an instance-hour comes back at a thousandth of a dollar and a
//!   GB-second far finer, each as the price list quotes it;
//! * the rows of one instance type arrive in the catalog's own pricing order,
//!   the dearest licence first, so every licensed platform precedes the
//!   unlicensed row;
//! * each row's `properties` arrive in the catalog's own order — the dimensions
//!   of the usage priced (tenancy, instance type, region, platform), and for an
//!   offering rate the commitment-scope dimension after them.
//!
//! Each rule is asserted over a table that spans the class rather than the one
//! combination that exposed it: the rows differ in plan type, term, payment
//! option, instance size, platform and product, and every expected price is
//! computed from the catalog's own published inputs
//! ([`state::EC2_INSTANCE_FAMILIES`], [`state::EC2_SIZES`],
//! [`state::EC2_PLATFORMS`]) rather than written down.
//!
//! Everything goes through the same `MultiServiceRouter` the emulator boots,
//! reusing the harness in [`crate::savings_plans_lifecycle`].

use serde_json::{json, Value};
use service_com_amazonaws_savingsplans_awssavingsplan::state;

use crate::savings_plans_lifecycle::{text, Emulator, ONE_YEAR, THREE_YEARS};

/// The region every row below is priced in: one the catalog sells in.
const REGION: &str = "us-east-1";

/// The instance family every EC2 row below prices, and the one the published
/// price-list anchors are quoted for.
const FAMILY: &str = "m5";

/// The dimensions a rate is priced along, in the order the answer reported them.
fn property_order(rate: &Value) -> Vec<String> {
    rate.get("properties")
        .and_then(Value::as_array)
        .map(|list| list.iter().map(|property| text(property, "name")).collect())
        .unwrap_or_default()
}

/// The value of one dimension of a rate, or `""`.
fn dimension(rate: &Value, name: &str) -> String {
    rate.get("properties")
        .and_then(Value::as_array)
        .and_then(|list| list.iter().find(|entry| text(entry, "name") == name))
        .map(|entry| text(entry, "value"))
        .unwrap_or_default()
}

/// Every row of one plan's rate card a request admits, walked through the
/// search's own cursor.
async fn card(emulator: &Emulator, case: &str, request: &Value) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut page = request.clone();
        page["maxResults"] = json!(1000);
        if let Some(token) = &next {
            page["nextToken"] = json!(token);
        }
        let answer = emulator.call("DescribeSavingsPlanRates", page.clone()).await;
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

/// Buy one offering of this plan type, payment option and term.
async fn buy(emulator: &Emulator, plan_type: &str, payment_option: &str, duration: i64) -> String {
    let offering = emulator.offering(plan_type, payment_option, duration).await;
    emulator
        .purchase(json!({
            "savingsPlanOfferingId": text(&offering, "offeringId"),
            "commitment": "0.5",
        }))
        .await
}

/// The on-demand price of one hour of `instance_type` on the platform whose
/// licence costs `licence_units`, in [`state::AMOUNT_SCALE`] units: the family's
/// per-vCPU price plus the platform's licence, over the size's vCPUs — the
/// catalog's own published inputs.
fn on_demand_units(size: &str, licence_units: i128) -> i128 {
    let vcpu_units = state::EC2_INSTANCE_FAMILIES
        .iter()
        .find(|(family, _)| *family == FAMILY)
        .map(|(_, units)| *units)
        .unwrap_or_else(|| panic!("the catalog prices the {FAMILY} family"));
    let vcpus = state::EC2_SIZES
        .iter()
        .find(|(name, _)| *name == size)
        .map(|(_, vcpus)| *vcpus)
        .unwrap_or_else(|| panic!("the catalog prices the {size} size"));
    (vcpu_units + licence_units) * vcpus
}

/// `on_demand` less `discount` basis points, quoted at the precision
/// `on_demand` is published at — the rule in this suite's OWN arithmetic rather
/// than through the helpers the handler prices with, so neither side of the
/// assertion can be satisfied by the other's formula.
fn expected_price_units(on_demand: i128, discount: i128) -> i128 {
    // The precision the published price carries: the coarsest power of ten it
    // is an exact multiple of.
    let mut quoted_unit = state::AMOUNT_SCALE;
    while quoted_unit > 1 && on_demand % quoted_unit != 0 {
        quoted_unit /= 10;
    }
    // Nearest quoted unit, halves up, and never nothing: a discounted price is
    // still a price.
    let retained = on_demand * (10_000 - discount);
    let per_unit = quoted_unit * 10_000;
    ((retained + per_unit / 2) / per_unit * quoted_unit).max(quoted_unit)
}

/// A row's rate is the catalog's price for that row's own dimensions, quoted at
/// the precision the catalog holds it, and the rows and their properties arrive
/// in the catalog's order.
///
/// The rows span the commitment attributes the discount follows (plan type, term
/// and payment option) and the instance sizes the family is sold in; within each,
/// every platform the catalog carries is checked.
#[tokio::test]
async fn a_rate_card_row_reports_the_catalogs_quoted_price_for_its_own_dimensions() {
    let emulator = Emulator::new();

    for (plan_type, payment_option, duration) in [
        ("Compute", state::PAYMENT_ALL_UPFRONT, ONE_YEAR),
        ("Compute", state::PAYMENT_NO_UPFRONT, ONE_YEAR),
        ("Compute", state::PAYMENT_PARTIAL_UPFRONT, THREE_YEARS),
        ("EC2Instance", state::PAYMENT_ALL_UPFRONT, ONE_YEAR),
        ("EC2Instance", state::PAYMENT_NO_UPFRONT, THREE_YEARS),
    ] {
        let savings_plan_id = buy(&emulator, plan_type, payment_option, duration).await;
        let discount = state::discount_basis_points(plan_type, duration, payment_option);

        for (size, _) in state::EC2_SIZES {
            let instance_type = format!("{FAMILY}.{size}");
            let case = format!("{plan_type}/{payment_option}/{duration} {instance_type}");
            let rows = card(
                &emulator,
                &case,
                &json!({
                    "savingsPlanId": savings_plan_id,
                    "filters": [
                        { "name": "region", "values": [REGION] },
                        { "name": "instanceType", "values": [instance_type] },
                    ],
                }),
            )
            .await;
            assert_eq!(
                rows.len(),
                state::EC2_PLATFORMS.len(),
                "case {case:?}: the card prices every platform of the instance type, got {} rows",
                rows.len()
            );

            for (index, rate) in rows.iter().enumerate() {
                let description = dimension(rate, state::RATE_PROPERTY_PRODUCT_DESCRIPTION);
                let licence = state::EC2_PLATFORMS
                    .iter()
                    .find(|(published, _, _)| *published == description)
                    .map(|(_, _, licence)| *licence)
                    .unwrap_or_else(|| {
                        panic!("case {case:?} row {index}: {description:?} is a platform the catalog carries")
                    });

                // The price of THIS row's dimension tuple, quoted at the
                // precision the catalog quotes this product's prices at.
                let on_demand = on_demand_units(size, licence);
                let expected = expected_price_units(on_demand, discount);
                let row = format!("{case} / {description}");
                let rendered = text(rate, "rate");
                assert_eq!(
                    state::parse_amount_at_wire_scale(&rendered),
                    Some(expected),
                    "case {row:?}: expected the catalog's price for this combination \
                     ({expected} units), got {rendered:?}"
                );
                // And spelled as the catalog holds it: no padding. The digits
                // are counted off the rendering itself, so the shortest exact
                // spelling of the value is what the answer carries.
                let fraction = rendered
                    .split_once('.')
                    .map(|(_, fraction)| fraction.to_string())
                    .unwrap_or_default();
                assert!(
                    !fraction.is_empty()
                        && (fraction.len() == 1 || !fraction.ends_with('0'))
                        && fraction.len() <= state::AMOUNT_WIRE_DIGITS,
                    "case {row:?}: a price carries the digits the catalog holds and no padding, \
                     got {rendered:?}"
                );
                // The dimensions of the usage priced, in the catalog's order.
                assert_eq!(
                    property_order(rate),
                    vec![
                        state::RATE_PROPERTY_TENANCY.to_string(),
                        state::RATE_PROPERTY_INSTANCE_TYPE.to_string(),
                        state::PROPERTY_REGION.to_string(),
                        state::RATE_PROPERTY_PRODUCT_DESCRIPTION.to_string(),
                    ],
                    "case {row:?}: expected the catalog's property order, got {:?}",
                    property_order(rate)
                );
            }

            // The catalog's row order: the dearest licence first, so every
            // licensed platform precedes the unlicensed one.
            let licences: Vec<i128> = rows
                .iter()
                .map(|rate| {
                    let description = dimension(rate, state::RATE_PROPERTY_PRODUCT_DESCRIPTION);
                    state::EC2_PLATFORMS
                        .iter()
                        .find(|(published, _, _)| *published == description)
                        .map(|(_, _, licence)| *licence)
                        .unwrap_or_default()
                })
                .collect();
            for (index, window) in licences.windows(2).enumerate() {
                assert!(
                    window[0] >= window[1],
                    "case {case:?}: the card lists the dearer platform first, got {} at row \
                     {index} and {} after it",
                    window[0],
                    window[1]
                );
            }
            let unlicensed = licences
                .iter()
                .position(|licence| *licence == 0)
                .unwrap_or_default();
            assert!(
                licences[..unlicensed].iter().all(|licence| *licence > 0),
                "case {case:?}: every licensed platform precedes the unlicensed row, got \
                 {licences:?}"
            );
        }
    }
}

/// A rate is reported as AWS's published price list quotes it.
///
/// The rows are entries of that price list for one instance family in one
/// region under a one-year All Upfront Compute plan: AWS publishes an `m5.large`
/// in `us-east-1` at $0.096 an hour on unlicensed Linux, $0.193 with SQL Server
/// Web and $0.928 on Windows with SQL Server Standard, and that commitment is
/// priced 31.25 percent below on-demand — so the rate comes back at a
/// thousandth of a dollar, the precision the price list quotes an instance-hour
/// at. The last two rows are a size and a product the price list quotes
/// differently: four times the vCPUs, and a Lambda GB-second (published at
/// $0.00001667) quoted eight places out.
#[tokio::test]
async fn a_published_price_is_reported_as_the_price_list_quotes_it() {
    let emulator = Emulator::new();
    let savings_plan_id = buy(&emulator, "Compute", state::PAYMENT_ALL_UPFRONT, ONE_YEAR).await;

    for (case, filters, expected) in [
        (
            "unlicensed Linux on m5.large",
            json!([
                { "name": "region", "values": [REGION] },
                { "name": "instanceType", "values": ["m5.large"] },
                { "name": "productDescription", "values": [state::PRODUCT_DESCRIPTION_LINUX] },
            ]),
            "0.066",
        ),
        (
            "Linux with SQL Server Web on m5.large",
            json!([
                { "name": "region", "values": [REGION] },
                { "name": "instanceType", "values": ["m5.large"] },
                { "name": "productDescription", "values": ["Linux with SQL Server Web"] },
            ]),
            "0.133",
        ),
        (
            "Windows with SQL Server Standard on m5.large",
            json!([
                { "name": "region", "values": [REGION] },
                { "name": "instanceType", "values": ["m5.large"] },
                { "name": "productDescription", "values": ["Windows with SQL Server Standard"] },
            ]),
            "0.638",
        ),
        (
            "Windows on m5.large",
            json!([
                { "name": "region", "values": [REGION] },
                { "name": "instanceType", "values": ["m5.large"] },
                { "name": "productDescription", "values": ["Windows"] },
            ]),
            "0.129",
        ),
        (
            "unlicensed Linux on m5.2xlarge",
            json!([
                { "name": "region", "values": [REGION] },
                { "name": "instanceType", "values": ["m5.2xlarge"] },
                { "name": "productDescription", "values": [state::PRODUCT_DESCRIPTION_LINUX] },
            ]),
            "0.264",
        ),
        (
            "a Lambda GB-second",
            json!([
                { "name": "region", "values": [REGION] },
                { "name": "productType", "values": ["Lambda"] },
            ]),
            "0.00001146",
        ),
    ] {
        let rows = card(
            &emulator,
            case,
            &json!({ "savingsPlanId": savings_plan_id, "filters": filters }),
        )
        .await;
        assert_eq!(
            rows.len(),
            1,
            "case {case:?}: expected the one row this combination is priced by, got {rows:?}"
        );
        assert_eq!(
            text(&rows[0], "rate"),
            expected,
            "case {case:?}: expected the price list's own value, got {:?}",
            text(&rows[0], "rate")
        );
    }
}

/// The offering rate card quotes the same catalog: one line item's price is the
/// same under the offering and under the plan bought from it, spelled the same
/// way, and the offering rate reports the commitment-scope dimension after the
/// dimensions of the usage priced.
#[tokio::test]
async fn an_offering_rate_quotes_the_same_catalog_as_the_plan_bought_from_it() {
    let emulator = Emulator::new();

    for (plan_type, payment_option, duration) in [
        ("Compute", state::PAYMENT_ALL_UPFRONT, ONE_YEAR),
        ("EC2Instance", state::PAYMENT_NO_UPFRONT, THREE_YEARS),
    ] {
        let case = format!("{plan_type}/{payment_option}/{duration}");
        let offering = emulator.offering(plan_type, payment_option, duration).await;
        let offering_id = text(&offering, "offeringId");
        let savings_plan_id = emulator
            .purchase(json!({
                "savingsPlanOfferingId": offering_id,
                "commitment": "0.5",
            }))
            .await;

        let filters = json!([
            { "name": "region", "values": [REGION] },
            { "name": "instanceType", "values": ["m5.large"] },
            { "name": "productDescription", "values": [state::PRODUCT_DESCRIPTION_LINUX] },
        ]);
        let plan_rows = card(
            &emulator,
            &case,
            &json!({ "savingsPlanId": savings_plan_id, "filters": filters }),
        )
        .await;
        let answer = emulator
            .call(
                "DescribeSavingsPlansOfferingRates",
                json!({
                    "savingsPlanOfferingIds": [offering_id],
                    "products": ["EC2"],
                    "filters": filters,
                    "maxResults": 1000,
                }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        let offering_rows = answer.list("searchResults");
        assert_eq!(
            (plan_rows.len(), offering_rows.len()),
            (1, 1),
            "case {case:?}: one line item is priced once by each, got {} and {}",
            plan_rows.len(),
            offering_rows.len()
        );
        assert_eq!(
            text(&plan_rows[0], "rate"),
            text(&offering_rows[0], "rate"),
            "case {case:?}: a plan's rate for a line item is the rate of the offering it was \
             bought from, got {:?} and {:?}",
            text(&plan_rows[0], "rate"),
            text(&offering_rows[0], "rate")
        );
        assert_eq!(
            property_order(&offering_rows[0]),
            vec![
                state::RATE_PROPERTY_TENANCY.to_string(),
                state::RATE_PROPERTY_INSTANCE_TYPE.to_string(),
                state::PROPERTY_REGION.to_string(),
                state::RATE_PROPERTY_PRODUCT_DESCRIPTION.to_string(),
                state::PROPERTY_INSTANCE_FAMILY.to_string(),
            ],
            "case {case:?}: expected the catalog's property order with the commitment's own \
             scope last, got {:?}",
            property_order(&offering_rows[0])
        );
    }
}
