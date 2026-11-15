Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: cfc4a64aa1fece035f2324d3ed7346f0c5cffa22

# A commitment recommendation's estimatedOnDemandCost reports the cost BEFORE buying the Savings Plans, contradicting the member's own doc and the response's savings figure

_Filed: 2026-11-15T07:21:17Z_
_Operation: GetRecommendation | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `cfc4a64aa1fece035f2324d3ed7346f0c5cffa22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetRecommendation` builds `recommendedResourceDetails.computeSavingsPlans.costCalculation.pricing` in `crate::state::resource_details`, where `estimatedOnDemandCost` is assigned `round_cents(pricing.cost_before_discounts)` — the configuration's cost at public pricing, i.e. the on-demand cost BEFORE any commitment is bought, and the very same number the adjacent `monthlySavingsPlansEligibleCost` is given.

The model documents that member as the on-demand cost that remains AFTER the purchase. For the seeded `PurchaseSavingsPlans` row the one response therefore says: eligible usage 1314.00, commitment 876.00, savingsPercentage 33.33, `estimatedOnDemandCost` 1314.00 — i.e. after adopting the recommendation the caller is told it pays an 876.00 commitment AND 1314.00 of on-demand, 2190.00 in total, while the same response's `estimatedMonthlyCost` 1314.00 / `estimatedMonthlySavings` 438.00 say the recommendation takes the monthly bill from 1314.00 down to 876.00.

The crate's own model of that price states which of the two it is: `Pricing::purchased_commitment` documents that the usage costs `cost_before_discounts` at public pricing and the commitment takes `commitment_purchase_discount` off it, so `cost_before_discounts` is the pre-purchase figure. Assigning it to the "after buying" member makes the detail half of the answer contradict the money half, which the catalog doc claims cannot happen.

## Evidence

Smithy (the member doc that itself states what the value is) — `smithy_query(query="shape", shape="SavingsPlansPricing")`:
```
"estimatedOnDemandCost": {"target": "Double", "doc": "<p>Estimated On-Demand cost you will pay after buying the Savings Plans.</p>"}
```
beside `"monthlySavingsPlansEligibleCost": ... "<p>The cost of paying for the recommended Savings Plans monthly.</p>"` and `"estimatedMonthlyCommitment": ... "<p>Estimated monthly commitment for the Savings Plans.</p>"`.

The code — `services/service-com-amazonaws-costoptimizationhub-costoptimizationhubservice/src/state.rs:876-888`:
```
            cost_calculation: Some(SavingsPlansCostCalculation {
                pricing: Some(SavingsPlansPricing {
                    monthly_savings_plans_eligible_cost: Some(round_cents(
                        pricing.cost_before_discounts,
                    )),
                    estimated_monthly_commitment: Some(pricing.cost_after_discounts()),
                    savings_percentage: Some(percentage_of(
                        pricing.discounts_total(),
                        pricing.cost_before_discounts,
                    )),
                    estimated_on_demand_cost: Some(round_cents(pricing.cost_before_discounts)),
```
(the "after buying" member and the eligible-usage member are given the identical expression).

What the crate says `cost_before_discounts` is, for exactly this kind of row — `src/state.rs:342-346` (doc on `Pricing::purchased_commitment`):
```
    /// The price of a configuration whose savings-plans discount is the one
    /// BUYING the recommended commitment creates: the usage costs
    /// `cost_before_discounts` at public pricing and the commitment takes
    /// `commitment_purchase_discount` off it, so the configuration costs the
    /// difference — the commitment — to whoever adopts the recommendation.
```
and the row it prices — `src/state.rs:1641-1642`:
```
            current_pricing: Pricing::list(1_314.0),
            recommended_pricing: Pricing::purchased_commitment(1_314.0, 438.0),
```

The other half of the same response — `src/ops/get_recommendation.rs:261-268`:
```
            estimated_monthly_savings: Some(row.monthly_savings(mode)),
            estimated_monthly_cost: Some(row.monthly_cost(mode)),
...
            recommended_resource_details: Some(row.recommended_resource_details()),
```
with `monthly_savings` = current 1314.00 − recommended 876.00 = 438.00 (`src/state.rs:480-484`), and the catalog's claim that the two halves agree — `src/state.rs:1460-1467`: "every figure a recommendation reports is nevertheless computed from the prices stored here (so cost, saving and percentage agree with each other and with the `ResourceDetails` cost calculation)".

Test coverage: the commitment suite reads only the other two members of this structure — `tests/all/commitment_purchase_savings.rs:241-243` (`let eligible = money(pricing, "monthlySavingsPlansEligibleCost"); let commitment = money(pricing, "estimatedMonthlyCommitment");`) and asserts `:256-261` that `savings == eligible - commitment`; `grep -n estimatedOnDemandCost tests/` matches nothing, so this member is pinned by no test and correcting it changes no existing assertion.

## Steps to Reproduce

1. Boot the emulator, enroll (`UpdateEnrollmentStatus` `{"status": "Active"}`) and wait out the generation delay.
2. Call `GetRecommendation` with `{"recommendationId": "0ec38a3a-1a0a-4a4f-9d2a-1f1e9a7c0005"}` (the seeded `PurchaseSavingsPlans` row, src/state.rs:1608-1649).
3. Read `recommendedResourceDetails.computeSavingsPlans.costCalculation.pricing`: `monthlySavingsPlansEligibleCost` 1314.0, `estimatedMonthlyCommitment` 876.0, `savingsPercentage` 33.33 and `estimatedOnDemandCost` 1314.0 — the member documented as the On-Demand cost payable AFTER buying the plan carries the full pre-purchase on-demand cost, identical to the eligible-usage member.
4. Compare with the top-level members of the same response: `estimatedMonthlyCost` 1314.0 and `estimatedMonthlySavings` 438.0, i.e. 876.0 after adoption — the detail half reports 876.0 commitment + 1314.0 on-demand.
