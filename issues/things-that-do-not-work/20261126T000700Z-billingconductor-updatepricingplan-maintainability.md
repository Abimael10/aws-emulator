Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 7f8655e57ee3af29caa93a56efb96c146f534a85

# A pricing plan's Size is derived twice — once in UpdatePricingPlan, once in ListPricingPlans — while the sibling AssociatedPricingPlanCount has one state method

_Filed: 2026-11-26T00:07:00Z_
_Operation: UpdatePricingPlan | Class: maintainability_

> _Note: this issue was identified against the code base at commit `7f8655e57ee3af29caa93a56efb96c146f534a85`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The pricing plan `Size` member ("the pricing rules count that's currently associated with this pricing plan") is computed by the same four-line expression in two handlers: resolve `associated_pricing_rule_arns(plan, period)`, map the `Vec<String>` length through `i64::try_from(...).unwrap_or(i64::MAX)`, and fall back to `unwrap_or_default()` for the `None` (plan absent in that period) case.

The crate already single-homes the structurally identical counts for the two sibling resources:

- the rule side's `AssociatedPricingPlanCount` is one state method, `associated_pricing_plan_count(rule_arn, billing_period) -> i64`, which `UpdatePricingRule` and `ListPricingRules` both call;
- the billing-group side's `Size` is one state method, `billing_group_sizes_in(group_arns, billing_period)`, which `UpdateBillingGroup` and `ListBillingGroups` both call (that single-homing is what issue `20261027T182740Z-billingconductor-updatebillinggroup-maintainability.md` established).

Only the plan side has no such method, so the two copies are kept in agreement by hand — which both copies' own comments assert rather than enforce. The fix is to add `associated_pricing_rule_count(&self, plan_arn, billing_period) -> i64` to `state.rs` beside `associated_pricing_plan_count` and have both handlers call it. Every response byte and every existing test stays unchanged: the two copies already compute the same number.

## Evidence

Copy 1 — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/update_pricing_plan.rs:258-268:
```
        // `Size`: "The pricing rules count that's currently associated with this pricing
        // plan list." Derived from the association the same state method every other
        // reader of it uses, so the count cannot drift from the rules it counts — a plan
        // that is associated with no rule counts 0, ...
        let size = state
            .associated_pricing_rule_arns(&plan_arn, &crate::state::current_billing_period())
            .map(|arns| i64::try_from(arns.len()).unwrap_or(i64::MAX))
            .unwrap_or_default();
```

Copy 2 — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/list_pricing_plans.rs:251-254:
```
                let size = state
                    .associated_pricing_rule_arns(&plan.arn, &billing_period)
                    .map(|arns| i64::try_from(arns.len()).unwrap_or(i64::MAX))
                    .unwrap_or_default();
```

The second copy's own doc admits the agreement is hand-maintained — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/list_pricing_plans.rs:126-131:
```
/// ... `Size` is passed in because it is DERIVED from the plan's
/// pricing-rule associations in that same period rather than held on the record: ...
/// A plan associated with no rule counts 0, which is what it holds, rather than the
/// model's unenforced `@range(min: 1)` floor; that is the same answer `UpdatePricingPlan`
/// gives.
```

The single-homed sibling the plan side lacks — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state.rs:3982-3987:
```
    /// `AssociatedPricingPlanCount`: "The pricing plans count that this pricing rule
    /// is associated with" — in `billing_period`, the period the read is scoped to.
    pub fn associated_pricing_plan_count(&self, rule_arn: &str, billing_period: &str) -> i64 {
        i64::try_from(self.pricing_plans_referencing_rule(rule_arn, billing_period).len())
            .unwrap_or(i64::MAX)
    }
```
and its two callers: src/ops/update_pricing_rule.rs:240-241 and src/ops/list_pricing_rules.rs:264.

The billing-group equivalent, also one method with two callers — src/state.rs:3387 (`pub fn billing_group_sizes_in`), called from src/ops/update_billing_group.rs:395-402 and src/ops/list_billing_groups.rs:337.

Test coverage that pins the responses the fix must leave unchanged: tests/all/pricing_plan_update.rs, tests/all/pricing_plan_unchanged_update.rs, tests/all/pricing_plan_list.rs, tests/all/pricing_plan_billing_periods.rs, tests/all/pricing_rule_association_billing_periods.rs.

## Steps to Reproduce

1. Open services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/update_pricing_plan.rs and read lines 258-268: the plan's `Size` is computed inline from `associated_pricing_rule_arns`, `i64::try_from(..).unwrap_or(i64::MAX)` and `unwrap_or_default()`.
2. Open services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/list_pricing_plans.rs and read lines 251-254: the identical three-step computation appears again, for the period the listing is scoped to.
3. Open src/state.rs:3982 and confirm the sibling `AssociatedPricingPlanCount` is instead one state method (`associated_pricing_plan_count`) called from both src/ops/update_pricing_rule.rs:241 and src/ops/list_pricing_rules.rs:264, and that src/state.rs:3387 does the same for the billing-group `Size`.
4. Grep the crate for a plan-side counterpart: `rg 'associated_pricing_rule_count' services/service-com-amazonaws-billingconductor-awsbillingconductor/src` returns nothing, so the plan `Size` rule has no single home.
5. Change the `i64::MAX` saturation or the `unwrap_or_default()` fallback in one copy only and observe that `UpdatePricingPlan` and `ListPricingPlans` can then report different `Size` values for the same plan and period, with no compiler or test failure forcing the other copy along.
