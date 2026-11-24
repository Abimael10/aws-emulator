// SPDX-License-Identifier: CC0-1.0

//! What a recommendation's money members report, against the cost calculation
//! the SAME answer carries for the two resource configurations, driven
//! end-to-end over the wire (`awsJson1_0`) through the same multi-service router
//! the emulator boots.
//!
//! The rule under test: the estimated monthly saving of a recommendation is what
//! the resource costs today less what it would cost in the recommended
//! configuration, so a recommendation whose action is BUYING a commitment saves
//! what that commitment takes off the usage it covers. The caller's
//! `savingsEstimationMode` preference decides only whether the discounts a
//! resource already carries are netted out of those two costs — both of them, so
//! an existing discount cancels — and never whether the discount the recommended
//! action itself creates counts, because that discount is the benefit being
//! estimated rather than one the estimate is taken before or after.
//!
//! Each test is a table over every row of the catalog and every state of the
//! preference, so the rule is asserted across action types, resource types,
//! priced-with and priced-without an existing discount, and each reader that
//! reports the figures.

use serde_json::{json, Value};
use service_com_amazonaws_costoptimizationhub_costoptimizationhubservice::ops::types::shared_types::{
    ACTION_TYPE_VALUES, SAVINGS_ESTIMATION_MODE_VALUES,
};
use service_com_amazonaws_costoptimizationhub_costoptimizationhubservice::state::{
    Pricing, ResourceConfig, AFTER_DISCOUNTS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cost_optimization_hub::{
    enable_member_account_discount_visibility, enrolled, enrolled_router_with_discounts_visible,
    management_account_router_with_store, ok, Store,
};

/// Two money figures are the same figure: every reader rounds to cents, so
/// anything inside half a cent is the same value.
fn same_money(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 0.005
}

/// How many days of cost one monthly figure covers, for prorating a monthly
/// saving over a lookback period measured in days.
const DAYS_PER_MONTH: f64 = 30.0;

/// The `ActionType` values whose action is buying a COMMITMENT rather than
/// changing a resource: the two `Purchase…` values of the model's `ActionType`
/// enum (`ACTION_TYPE_VALUES`, read from the service so this list cannot drift
/// from the model's).
fn commitment_purchase_actions() -> Vec<&'static str> {
    ACTION_TYPE_VALUES
        .iter()
        .copied()
        .filter(|action| action.starts_with("Purchase"))
        .collect()
}

/// Every state of the `savingsEstimationMode` preference, in the order an
/// account moves through them: no preference at all first — read under the
/// default the service applies — then each value of the model's
/// `SavingsEstimationMode` enum.
fn modes() -> Vec<Option<&'static str>> {
    std::iter::once(None)
        .chain(SAVINGS_ESTIMATION_MODE_VALUES.iter().copied().map(Some))
        .collect()
}

/// Put the account into `mode`, leaving it under the service's default where the
/// case states no preference.
async fn store_mode(router: &MultiServiceRouter, mode: Option<&str>) {
    if let Some(mode) = mode {
        ok(
            router,
            "UpdatePreferences",
            &json!({"savingsEstimationMode": mode}),
        )
        .await;
    }
}

/// One money member of an answer.
fn money(payload: &Value, member: &str) -> f64 {
    payload[member]
        .as_f64()
        .unwrap_or_else(|| panic!("expected a number in {member}, got {payload}"))
}

/// The `costCalculation.pricing` of one half of a detail answer. The
/// `ResourceDetails` union carries exactly one member, and that member prices
/// the configuration it describes.
fn pricing_of(details: &Value) -> &Value {
    let resource = details
        .as_object()
        .and_then(|members| members.values().next())
        .unwrap_or_else(|| panic!("expected one resource in the details, got {details}"));
    &resource["costCalculation"]["pricing"]
}

/// What the caller pays per month for the configuration `details` describes, as
/// that half of the SAME answer prices it, under `mode`.
///
/// A commitment is priced by `SavingsPlansPricing`, which splits that cost into
/// the commitment the configuration's plan charges and the On-Demand cost that
/// commitment leaves uncovered, so the two together are what the configuration
/// costs; every other resource is priced by `ResourcePricing`, whose before- and
/// after-discount members are the two views of its existing discounts the mode
/// selects between.
fn reported_cost(details: &Value, mode: Option<&str>) -> f64 {
    let pricing = pricing_of(details);
    if let Some(commitment) = pricing["estimatedMonthlyCommitment"].as_f64() {
        return commitment + money(pricing, "estimatedOnDemandCost");
    }
    let member = if mode == Some(AFTER_DISCOUNTS) {
        "estimatedCostAfterDiscounts"
    } else {
        "estimatedCostBeforeDiscounts"
    };
    money(pricing, member)
}

/// Every recommendation the caller may read, as whole list entries, under the
/// filter `request` carries.
async fn listed(router: &MultiServiceRouter, request: &Value) -> Vec<Value> {
    let payload = ok(router, "ListRecommendations", request).await;
    payload["items"].as_array().cloned().unwrap_or_default()
}

/// Every recommendation's money members are the difference between the two cost
/// calculations the same answer carries, under every state of the preference —
/// so no row reports a saving its own `ResourceDetails` contradict.
///
/// The table is every row of the catalog (spanning action types, resource types,
/// detail kinds and prices with and without an existing discount) crossed with
/// every state of the mode preference.
#[tokio::test]
async fn money_members_agree_with_the_cost_calculations_of_the_same_answer() {
    let router = enrolled_router_with_discounts_visible().await;

    for mode in modes() {
        store_mode(&router, mode).await;
        let items = listed(&router, &json!({"includeAllRecommendations": true})).await;
        assert!(
            items.len() > 1,
            "case {mode:?}: expected the catalog to offer several recommendations, got {items:?}"
        );

        for item in &items {
            let recommendation_id = item["recommendationId"].as_str().unwrap_or_default();
            let case = format!("{recommendation_id} under {mode:?}");
            let detail = ok(
                &router,
                "GetRecommendation",
                &json!({"recommendationId": recommendation_id}),
            )
            .await;

            let cost = money(&detail, "estimatedMonthlyCost");
            let savings = money(&detail, "estimatedMonthlySavings");
            let current = reported_cost(&detail["currentResourceDetails"], mode);
            let recommended = reported_cost(&detail["recommendedResourceDetails"], mode);

            assert!(
                same_money(cost, current),
                "case {case:?}: expected the monthly cost to be what the current resource's \
                 cost calculation prices it at ({current}), got {cost}"
            );
            assert!(
                same_money(savings, current - recommended),
                "case {case:?}: expected the saving to be the current cost {current} less the \
                 recommended configuration's {recommended} ({}), got {savings}",
                current - recommended
            );
            assert!(
                same_money(
                    money(&detail, "estimatedSavingsPercentage"),
                    savings / cost * 100.0
                ),
                "case {case:?}: expected {} percent of the {cost} monthly cost, got {}",
                savings / cost * 100.0,
                money(&detail, "estimatedSavingsPercentage")
            );

            let lookback = money(&detail, "costCalculationLookbackPeriodInDays");
            let over_lookback = money(&detail, "estimatedSavingsOverCostCalculationLookbackPeriod");
            assert!(
                same_money(over_lookback, savings * lookback / DAYS_PER_MONTH),
                "case {case:?}: expected {} over {lookback} days, got {over_lookback}",
                savings * lookback / DAYS_PER_MONTH
            );

            // The list reader reports the same figures for the same row, so the
            // agreement is not one reader's alone.
            for member in [
                "estimatedMonthlyCost",
                "estimatedMonthlySavings",
                "estimatedSavingsPercentage",
            ] {
                assert!(
                    same_money(money(item, member), money(&detail, member)),
                    "case {case:?}: expected the list and detail views to report the same \
                     {member}, got {} and {}",
                    money(item, member),
                    money(&detail, member)
                );
            }
        }
    }
}

/// A recommendation whose action is buying a commitment saves what the
/// commitment takes off the usage it covers, under every state of the
/// preference: the benefit is the commitment itself, so no mode reports it as
/// saving nothing.
///
/// The table is every commitment-purchase row the catalog offers crossed with
/// every state of the mode preference.
#[tokio::test]
async fn a_commitment_purchase_saves_what_its_commitment_takes_off_in_every_mode() {
    let router = enrolled_router_with_discounts_visible().await;
    let purchase_actions = commitment_purchase_actions();

    for mode in modes() {
        store_mode(&router, mode).await;
        let items = listed(&router, &json!({"includeAllRecommendations": true})).await;
        let purchases: Vec<&Value> = items
            .iter()
            .filter(|item| {
                purchase_actions.contains(&item["actionType"].as_str().unwrap_or_default())
            })
            .collect();
        assert!(
            !purchases.is_empty(),
            "case {mode:?}: expected the catalog to offer a commitment-purchase recommendation \
             among {items:?}"
        );

        for item in purchases {
            let recommendation_id = item["recommendationId"].as_str().unwrap_or_default();
            let case = format!("{recommendation_id} under {mode:?}");
            let detail = ok(
                &router,
                "GetRecommendation",
                &json!({"recommendationId": recommendation_id}),
            )
            .await;

            let pricing = pricing_of(&detail["recommendedResourceDetails"]);
            let eligible = money(pricing, "monthlySavingsPlansEligibleCost");
            let commitment = money(pricing, "estimatedMonthlyCommitment");
            assert!(
                commitment < eligible,
                "case {case:?}: expected a recommended commitment below the {eligible} the \
                 eligible usage costs, got {commitment}"
            );

            let savings = money(&detail, "estimatedMonthlySavings");
            assert!(
                savings > 0.0,
                "case {case:?}: expected the commitment's benefit to be reported as a saving, \
                 got {savings}"
            );
            assert!(
                same_money(savings, eligible - commitment),
                "case {case:?}: expected the saving to be the {eligible} eligible usage less \
                 the {commitment} commitment ({}), got {savings}",
                eligible - commitment
            );
            assert!(
                same_money(
                    money(pricing, "savingsPercentage"),
                    (eligible - commitment) / eligible * 100.0
                ),
                "case {case:?}: expected the cost calculation to report {} percent, got {}",
                (eligible - commitment) / eligible * 100.0,
                money(pricing, "savingsPercentage")
            );
            assert!(
                money(&detail, "estimatedSavingsPercentage") > 0.0,
                "case {case:?}: expected a positive savings percentage, got {}",
                money(&detail, "estimatedSavingsPercentage")
            );
        }

        // The same saving reaches the summary reader: each commitment-purchase
        // action's group totals what that action's de-duped rows save, and the
        // whole view's total is at least that much.
        let whole_view = ok(&router, "ListRecommendationSummaries", &json!({"groupBy": "ActionType"})).await;
        for action in &purchase_actions {
            let filter = json!({"actionTypes": [action]});
            let deduped = listed(
                &router,
                &json!({"includeAllRecommendations": false, "filter": filter}),
            )
            .await;
            if deduped.is_empty() {
                continue;
            }
            let expected: f64 = deduped
                .iter()
                .map(|item| money(item, "estimatedMonthlySavings"))
                .sum();
            let case = format!("{action} under {mode:?}");
            assert!(
                expected > 0.0,
                "case {case:?}: expected the action's rows to save something, got {expected}"
            );

            let summary = ok(
                &router,
                "ListRecommendationSummaries",
                &json!({"groupBy": "ActionType", "filter": {"actionTypes": [action]}}),
            )
            .await;
            let group = money(&summary["items"][0], "estimatedMonthlySavings");
            assert!(
                same_money(group, expected),
                "case {case:?}: expected the group to total {expected}, got {group}"
            );
            assert!(
                same_money(money(&summary, "estimatedTotalDedupedSavings"), expected),
                "case {case:?}: expected the filtered total to be {expected}, got {}",
                money(&summary, "estimatedTotalDedupedSavings")
            );
            assert!(
                money(&whole_view, "estimatedTotalDedupedSavings") >= expected - 0.005,
                "case {case:?}: expected the whole view's total to include the {expected} this \
                 action saves, got {}",
                money(&whole_view, "estimatedTotalDedupedSavings")
            );
        }
    }
}

/// How many Savings Plans configurations one commitment-purchase answer prices:
/// the one the resource has today and the one the recommendation proposes.
const PLAN_HALVES_PER_ANSWER: usize = 2;

/// The hours of a month an hourly Savings Plans commitment is charged over:
/// an 8_760-hour year spread evenly, which is the rate the catalog's own
/// Savings Plans row prices its hourly commitment at.
const HOURS_PER_MONTH: f64 = 8_760.0 / 12.0;

/// How many rows of the catalog price a Savings Plans configuration, so the
/// coverage check below counts the rows the catalog holds rather than assuming
/// how many it seeds.
fn savings_plans_row_count(store: &Store) -> usize {
    let store = store.lock();
    store
        .recommendation_order
        .iter()
        .filter_map(|id| store.recommendations.get(id))
        .filter(|row| {
            matches!(
                row.recommended_resource_config,
                ResourceConfig::ComputeSavingsPlans { .. }
            )
        })
        .count()
}

/// Give the catalog more Savings Plans purchase rows — copies of the one it
/// seeds, re-priced at other commitment sizes — so the rule below is asserted
/// over several commitments rather than over one row. The emulator derives no
/// catalog from observed usage, so no operation creates them.
///
/// The re-priced usage carries no discount of its own, as the seeded row's does
/// not: `SavingsPlansPricing` has no before/after-discount pair for the savings
/// estimation mode to select between, so a Savings Plans configuration is
/// priced the same under every mode.
fn repriced_savings_plans_rows(store: &Store) -> Vec<String> {
    // (hourly commitment the plan charges, what the eligible usage costs a
    // month at public pricing, what the commitment takes off it). The discount
    // of each row leaves a commitment of the hourly rate over a month of
    // HOURS_PER_MONTH hours, as the catalog's own row does, so the
    // configuration a row describes is one that could exist.
    let cases = [
        ("0.30", 438.0, 438.0 - 0.30 * HOURS_PER_MONTH),
        ("2.50", 7_300.0, 7_300.0 - 2.50 * HOURS_PER_MONTH),
    ];

    let mut store = store.lock();
    let template = store
        .recommendation_order
        .iter()
        .filter_map(|id| store.recommendations.get(id))
        .find(|row| {
            matches!(
                row.recommended_resource_config,
                ResourceConfig::ComputeSavingsPlans { .. }
            )
        })
        .cloned()
        .expect("the catalog seeds a Savings Plans purchase recommendation");
    let ResourceConfig::ComputeSavingsPlans {
        ref term,
        ref payment_option,
        ..
    } = template.recommended_resource_config
    else {
        unreachable!("the row found is the one whose recommended configuration is a plan")
    };

    let mut created = Vec::new();
    for (hourly_commitment, eligible, discount) in cases {
        let mut row = template.clone();
        row.recommendation_id = format!("{}-at-{hourly_commitment}", template.recommendation_id);
        row.resource_id = format!("{}-at-{hourly_commitment}", template.resource_id);
        row.resource_arn = format!("{}-at-{hourly_commitment}", template.resource_arn);
        row.recommended_resource_summary =
            format!("{term}, {payment_option}, {hourly_commitment} USD/hour");
        row.recommended_resource_config = ResourceConfig::ComputeSavingsPlans {
            term: term.clone(),
            payment_option: payment_option.clone(),
            hourly_commitment: hourly_commitment.to_string(),
        };
        row.current_pricing = Pricing {
            cost_before_discounts: eligible,
            ..Pricing::default()
        };
        row.recommended_pricing = Pricing {
            cost_before_discounts: eligible,
            commitment_purchase_discount: discount,
            ..Pricing::default()
        };
        created.push(row.recommendation_id.clone());
        store.recommendation_order.push(row.recommendation_id.clone());
        store
            .recommendations
            .insert(row.recommendation_id.clone(), row);
    }
    created
}

/// A Savings Plans cost calculation splits what the configuration it prices
/// costs per month into the commitment that configuration's plan charges and
/// the On-Demand cost the commitment leaves uncovered: so a configuration that
/// commits to no plan reports no commitment, and a configuration whose plan
/// covers its eligible usage reports an On-Demand cost BELOW what that usage
/// costs at public pricing — never the pre-purchase cost itself, which is the
/// adjacent eligible-cost member. The two parts add up to what the same
/// answer's money members say the configuration costs, so the detail half of an
/// answer cannot report a bill the money half contradicts.
///
/// The table is every Savings Plans row of the catalog — the seeded one plus
/// rows re-priced at a smaller and a larger commitment — crossed with both
/// halves of each answer (the configuration with no commitment and the one that
/// buys one) and every state of the mode preference.
#[tokio::test]
async fn a_savings_plans_detail_splits_its_cost_into_commitment_and_on_demand() {
    let (router, store) = management_account_router_with_store();
    let router = enrolled(router).await;
    enable_member_account_discount_visibility(&router).await;
    let repriced = repriced_savings_plans_rows(&store);
    assert!(
        !repriced.is_empty(),
        "case setup: expected the catalog to take the re-priced Savings Plans rows"
    );
    let expected_halves = savings_plans_row_count(&store) * PLAN_HALVES_PER_ANSWER;

    for mode in modes() {
        store_mode(&router, mode).await;
        let items = listed(&router, &json!({"includeAllRecommendations": true})).await;
        let mut priced_halves = 0_usize;

        for item in &items {
            let recommendation_id = item["recommendationId"].as_str().unwrap_or_default();
            let detail = ok(
                &router,
                "GetRecommendation",
                &json!({"recommendationId": recommendation_id}),
            )
            .await;
            let cost = money(&detail, "estimatedMonthlyCost");
            let savings = money(&detail, "estimatedMonthlySavings");

            // Each half of the answer, beside what the SAME answer's money
            // members say that configuration costs a month: what the resource
            // costs today, and what adopting the recommendation leaves of it.
            for (half, configuration_cost) in [
                ("currentResourceDetails", cost),
                ("recommendedResourceDetails", cost - savings),
            ] {
                let Some(plan) = detail[half].get("computeSavingsPlans") else {
                    continue;
                };
                priced_halves += 1;
                let case = format!("{recommendation_id} {half} under {mode:?}");
                let pricing = &plan["costCalculation"]["pricing"];
                let eligible = money(pricing, "monthlySavingsPlansEligibleCost");
                let commitment = money(pricing, "estimatedMonthlyCommitment");
                let on_demand = money(pricing, "estimatedOnDemandCost");
                let hourly = plan["configuration"]["hourlyCommitment"]
                    .as_str()
                    .and_then(|text| text.parse::<f64>().ok())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected an hourly commitment, got {plan}")
                    });

                assert!(
                    same_money(commitment + on_demand, configuration_cost),
                    "case {case:?}: expected the {commitment} commitment and the {on_demand} \
                     On-Demand cost left after it to be the {configuration_cost} this \
                     configuration costs, got {}",
                    commitment + on_demand
                );
                assert!(
                    on_demand <= eligible + 0.005,
                    "case {case:?}: expected at most the {eligible} the eligible usage costs at \
                     public pricing to remain On-Demand, got {on_demand}"
                );
                if hourly == 0.0 {
                    assert!(
                        same_money(commitment, 0.0),
                        "case {case:?}: expected no commitment for a configuration that buys \
                         none, got {commitment}"
                    );
                } else {
                    assert!(
                        commitment > 0.0,
                        "case {case:?}: expected the {hourly} an hour plan to report a \
                         commitment, got {commitment}"
                    );
                    assert!(
                        on_demand < eligible - 0.005,
                        "case {case:?}: expected the On-Demand cost left after buying the plan \
                         to be below the {eligible} that usage costs before it, got {on_demand}"
                    );
                }
            }
        }

        assert_eq!(
            priced_halves, expected_halves,
            "case {mode:?}: expected {expected_halves} Savings Plans configurations priced across \
             the catalog, got {priced_halves}"
        );
    }
}

/// The same split at the price itself, over a table of prices: the commitment
/// and the On-Demand remainder are the two parts of what a configuration costs
/// a month, so a price carrying no purchased commitment commits nothing and
/// stays On-Demand in full, and one carrying a purchased commitment leaves an
/// On-Demand cost below what that usage costs at public pricing.
///
/// The rows differ in the size of the usage, the size of the commitment and
/// whether the usage carries discounts of its own, and include the zero-cost
/// price a configuration the recommendation would delete is priced at.
#[test]
fn every_price_splits_into_a_commitment_and_an_on_demand_remainder() {
    // (case, the price)
    let cases: [(&str, Pricing); 6] = [
        ("no cost at all", Pricing::default()),
        (
            "usage at public pricing, under no plan",
            Pricing {
                cost_before_discounts: 1_314.0,
                ..Pricing::default()
            },
        ),
        (
            "usage carrying discounts of its own, under no plan",
            Pricing {
                cost_before_discounts: 2_000.0,
                savings_plans_discount: 300.0,
                reserved_instances_discount: 120.0,
                other_discount: 80.0,
                ..Pricing::default()
            },
        ),
        (
            "a commitment covering a month of usage",
            Pricing {
                cost_before_discounts: 1_314.0,
                commitment_purchase_discount: 438.0,
                ..Pricing::default()
            },
        ),
        (
            "a commitment taking off all but a cent",
            Pricing {
                cost_before_discounts: 10.0,
                commitment_purchase_discount: 9.99,
                ..Pricing::default()
            },
        ),
        (
            "a commitment on usage that carries a discount of its own",
            Pricing {
                cost_before_discounts: 1_000.0,
                other_discount: 100.0,
                commitment_purchase_discount: 250.0,
                ..Pricing::default()
            },
        ),
    ];

    for (case, price) in cases {
        let commitment = price.monthly_commitment();
        let on_demand = price.on_demand_cost_after_commitment();
        let total = price.cost_after_discounts();
        assert!(
            same_money(commitment + on_demand, total),
            "case {case:?}: expected the {commitment} commitment and the {on_demand} On-Demand \
             remainder to be the {total} the configuration costs, got {}",
            commitment + on_demand
        );

        if price.commitment_purchase_discount > 0.0 {
            assert!(
                commitment > 0.0,
                "case {case:?}: expected a purchased commitment to be reported, got {commitment}"
            );
            assert!(
                on_demand < price.cost_before_discounts - 0.005,
                "case {case:?}: expected the On-Demand remainder to be below the {} the usage \
                 costs at public pricing, got {on_demand}",
                price.cost_before_discounts
            );
        } else {
            assert!(
                same_money(commitment, 0.0),
                "case {case:?}: expected no commitment where no plan is bought, got {commitment}"
            );
            assert!(
                same_money(on_demand, total),
                "case {case:?}: expected the whole {total} to stay On-Demand, got {on_demand}"
            );
        }
    }
}
