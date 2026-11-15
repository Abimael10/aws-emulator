Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: a22dc19d5e32998b957368c08025d606fc68b0e9

# The charge summary places a billing month on the pricing-plan timeline by the month's FIRST INSTANT only, so a plan that took effect part-way through the month is answered 404 while GetEnterpriseSupportContractDetails reports it active for that very month

_Filed: 2026-11-15T04:30:49Z_
_Operation: GetEnterpriseSupportChargeSummary | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `a22dc19d5e32998b957368c08025d606fc68b0e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetEnterpriseSupportChargeSummary` resolves the plan a month is priced by with `EnterpriseSupportContract::effective_pricing_plan(<first instant of the month>)`, a POINT-IN-TIME predicate (`covers_instant`). Its sibling read of the same stored timeline, `GetEnterpriseSupportContractDetails`, resolves the same month with `pricing_plans_for_period(<month start>, <month end>)`, an OVERLAP predicate (`covers_period`) whose doc in this crate states the rule explicitly: a plan that starts or ends inside a billing month IS in effect for that month.

Two consequences follow from the same stored contract:

1. A plan whose `start_date` falls part-way through the requested month covers no part of the month's first instant, so `effective_pricing_plan` answers `None` and the summary refuses the month with the modeled `ResourceNotFoundException` (404) — "No Enterprise Support pricing plan is in effect for billing month YYYY-MM" — even though `GetEnterpriseSupportContractDetails` answers 200 for the same month with `isContractActive: true` and that very plan in `pricingPlans`, and even though the store holds Support-eligible spend for the month. Two reads of one month disagree about whether the contract was in effect.

2. A plan that ENDED part-way through the month still covers the first instant, so the whole month's spend is priced by it and `supportEffectivePricingPlan` reports it, while the successor plan that took effect mid-month — which `GetEnterpriseSupportContractDetails` lists for the same month — prices nothing.

The handler's own comment grounds the refusal on "A month no stored plan covers was not billed under this contract", but a plan starting mid-month DOES cover part of the month, which is exactly what the crate's `covers_period` doc says makes it in effect for the month.

## Evidence

Summary places the month by its first instant and 404s otherwise — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_charge_summary.rs:311-334:
```
        // `supportEffectivePricingPlan` is "the effective pricing plan used for
        // the support charge calculation" — the one the contract was billed
        // under in the requested billing period. A month no stored plan covers
        // was not billed under this contract, so there is no charge to report
        // for it.
        let plan = state
            .enterprise_support_contract
            .effective_pricing_plan(crate::state::date_time_secs(&billing_period_start_date))
            .ok_or_else(|| { ... ResourceNotFoundException {
                            message: format!(
                                "No Enterprise Support pricing plan is in effect for billing \
                                 month {}",
                                input.billing_month
                            ),
```
That resolver is point-in-time — services/service-com-amazonaws-billing-awsbilling/src/state/enterprise_support.rs:469-476:
```
    pub fn effective_pricing_plan(&self, at_secs: f64) -> Option<&EnterpriseSupportPricingPlan> {
        self.pricing_plans
            .iter()
            .filter(|plan| plan.covers_instant(at_secs))
```
The crate's own stated rule for placing a plan in a billing month — services/service-com-amazonaws-billing-awsbilling/src/state/enterprise_support.rs:279-288:
```
    /// Whether the plan is in effect anywhere in the half-open period
    /// `from_secs..to_secs`: its own period overlaps that one, so a plan that
    /// starts or ends inside a billing month is in effect for that month even
    /// though it does not cover every instant of it.
    pub fn covers_period(&self, from_secs: f64, to_secs: f64) -> bool {
```
The sibling read applies that rule to the same month — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_contract_details.rs:288-303:
```
        let plans = contract.pricing_plans_for_period(
            crate::state::date_time_secs(&period_start),
            crate::state::date_time_secs(&period_end),
        );
...
            // "When true, the Enterprise Support contract is active. ...
            is_contract_active: Some(!plans.is_empty()),
```
Test coverage: tests/all/enterprise_support_charge_summary.rs:649-726 (`prices_a_month_by_the_pricing_plan_in_effect_for_it`) only ever stores plans starting exactly at a month boundary (`negotiated_plan(PAST_MONTH_START_SECS, ...)`), and tests/all/enterprise_support_charge_summary.rs:868-888 (`refuses_a_month_no_pricing_plan_covers`) ends its plan exactly at `PAST_MONTH_START_SECS` with the stated reason "a plan that ended covers the months before it ended and no later one" — a case both predicates agree on. No test pins the mid-month case either way.

## Steps to Reproduce

1. Seed Support-eligible spend for a past billing month for the caller (the payer account), e.g. `record_support_eligible_spend` for 2024-03 with the Enterprise plan.
2. Replace `state.enterprise_support_contract.pricing_plans` with a single plan whose `start_date` is the 10th of 2024-03 (part-way through the month) and no `end_date`.
3. Call `GetEnterpriseSupportContractDetails` with `billingMonth: "2024-03"` — 200, `isContractActive: true`, `pricingPlans` lists that plan.
4. Call `GetEnterpriseSupportChargeSummary` with the same `billingMonth` — 404 `ResourceNotFoundException` "No Enterprise Support pricing plan is in effect for billing month 2024-03", though the same month's contract and spend are both stored.
5. Inverse case: store plan A ending on the 10th and plan B starting on the 10th. `GetEnterpriseSupportContractDetails` lists both for the month; `GetEnterpriseSupportChargeSummary` prices the whole month under A and reports A as `supportEffectivePricingPlan`.
