Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 0607523cc4a05d467d979a52c3af01f5efd82726

# DeletePricingPlan erases the plan from every EARLIER billing period it existed in, breaking the crate's own "a delete that names no range ends records from the billing period the call is made in and never earlier" rule

_Filed: 2026-11-22T04:28:40Z_
_Operation: DeletePricingPlan | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `0607523cc4a05d467d979a52c3af01f5efd82726`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A pricing plan is a billing-period-scoped record in this crate: it carries generations (`PricingPlanVersion`), the crate answers "did this plan exist in this billing period" (`pricing_plan_exists_in`), and `ListPricingPlans` filters on exactly that (`plan.exists_in(&billing_period)`), reporting the generation and the `Size` that held in the month asked about.

`DeletePricingPlan` applies no billing-period bound at all: it calls `state.remove_pricing_plan(&plan_arn)`, which does `self.pricing_plans.remove(plan_arn)`. Because every period-scoped read of a plan walks `state.pricing_plans`, the plan instantly disappears from EVERY earlier billing period too — months it really existed in and really computed a bill with. Observable retroactive loss:

* `ListPricingPlans` with `BillingPeriod` = a month before the delete reported the plan (with its `Size`) and now reports nothing;
* `ListPricingRulesAssociatedToPricingPlan` for that same earlier month answered 200 with the rules the plan associated then and now answers the modelled 404 `ResourceNotFoundException`;
* `ListPricingPlansAssociatedWithPricingRule` for a rule the plan associated in that month reported the plan and now omits the `PricingPlanArns` member entirely.

That contradicts the rule this crate states in universal terms and implements for both sibling resources. `remove_billing_group` keeps the deleted group as a `DeletedBillingGroupRecord` so "a read scoped to one of the periods before the delete still reports the group as it stood then", and calls the rule general while ending the group's custom line items at `BillingPeriodSpan::ending_now()`. `DeleteCustomLineItem` with no range narrows rather than removes, for the same stated reason. Only the two pricing-resource deletes drop the record whole.

Note the asymmetry inside the plan-delete gate itself: `billing_groups_referencing_pricing_plan` is careful to consult EVERY generation of every stored AND deleted billing group precisely because a closed period still reports what it computed with — yet the delete that gate guards then erases the plan from those very closed periods for every reader that does not go through a billing group.

## Evidence

The handler applies no period bound — `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/delete_pricing_plan.rs:256-259`:

        // One state method removes the plan and the index entries that only existed
        // because of it: its name-index entry (so the name can be reused) and the
        // idempotency entries that named it.
        state.remove_pricing_plan(&plan_arn);

and that method deletes the record from the map every period-scoped read walks — `src/state.rs:3886-3888`:

    pub fn remove_pricing_plan(&mut self, plan_arn: &str) -> Option<PricingPlanRecord> {
        let removed = self.pricing_plans.remove(plan_arn)?;
        self.pricing_plan_names.remove(&removed.name);

A plan IS a billing-period-scoped record — `src/state.rs:4007-4013` (`pricing_plan_exists_in`):

    /// Did the pricing plan `plan_arn` exist in `billing_period` — is it one of the plans
    /// a read scoped to that period reports?
    ...
    /// [`Self::pricing_rule_exists_in`]: a stored plan exists from the period it was
    /// created in on ([`VersionedResource::exists_in`]) ...

and `ListPricingPlans` reports it on exactly that basis — `src/ops/list_pricing_plans.rs:207-213` and `:225-233`:

        // `BillingPeriod` selects the month the listing is READ FOR ... So a plan is
        // reported only for the periods it existed in, with the values that held in the
        // period asked about
        ...
            .filter(|(arn, plan)| {
                wanted.as_ref().is_none_or(|wanted| wanted.contains(*arn))
                    && plan.exists_in(&billing_period)
            })

The SAME crate, for the sibling resource and the same "a delete that names no range" case, does the opposite — `src/state.rs:3530-3537` and `:3544-3554`:

    /// What the delete does NOT erase is the group's own record of the billing periods
    /// it billed in: the removed record is kept as a
    /// [`DeletedBillingGroupRecord`](crate::state::DeletedBillingGroupRecord) ... so ...
    /// a read scoped to one of the periods before the delete still reports the group as it
    /// stood then ([`Self::billing_groups_in`])

        // A custom line item "applies to" one billing group, so it cannot outlive it —
        // and, like every other billing-period-scoped record a delete that names no
        // range of its own ends, it stops applying FROM the billing period the delete is
        // made in and never earlier ([`BillingPeriodSpan::ending_now`]): the periods
        // before it are closed bills the item really carried its charge in

The crate's own suite states the rule as general — `tests/all/deleted_billing_group_keeps_its_period_charges.rs:10-16`:

    //! RULE: a delete that names no billing-period range ends every billing-period-scoped
    //! record it takes with it FROM the billing period the call is made in and never earlier,
    //! so a read scoped to a period before the delete reports and prices the group's custom
    //! line items and their resource associations exactly as it did before the delete

And the plan-delete gate itself depends on closed periods still reporting what they computed with — `src/ops/delete_pricing_plan.rs:211-218`:

        // Every GENERATION a period-scoped read of the groups can report counts, not
        // just the one in force now ... including the
        // generations of a group a delete has since removed, which the periods before
        // that delete go on reporting.
        let holders = state.billing_groups_referencing_pricing_plan(&plan_arn);

Test coverage: `tests/all/pricing_plan_delete.rs:6-10` pins the whole-record removal ("removes from state the ONE pricing plan its `Arn` identifies ... frees its name for a later create") and uses `ListPricingRulesAssociatedToPricingPlan`'s 404 as its existence probe, but its stated reason never engages the billing-period question; no test in `tests/all/pricing_plan_billing_periods.rs` or `tests/all/pricing_plan_list.rs` asks what an EARLIER billing period reports after a `DeletePricingPlan`.

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as every test harness in this crate does: `engine.state::<...State>().lock().payer_account_id = Some(emulator_account_id())`).
2. POST /create-pricing-rule with {"Name":"Rule1","Scope":"GLOBAL","Type":"MARKUP","ModifierPercentage":10} and keep the ARN R.
3. POST /create-pricing-plan with {"Name":"Plan1","PricingRuleArns":[R]} and keep the ARN P. Associate no billing group with P.
4. Backdate the plan's and the association's stored instants by one month (the way `tests/all/pricing_plan_billing_periods.rs` and `tests/all/pricing_rule_association_periods.rs` do), so P existed and associated R in the PREVIOUS billing period M1.
5. POST /list-pricing-plans with {"BillingPeriod":M1} -> P is reported with Size 1; POST /list-pricing-rules-associated-to-pricing-plan with {"PricingPlanArn":P,"BillingPeriod":M1} -> 200 with [R]; POST /list-pricing-plans-associated-with-pricing-rule with {"PricingRuleArn":R,"BillingPeriod":M1} -> 200 with [P].
6. POST /delete-pricing-plan with {"Arn":P} -> HTTP 200 (no billing group names P, so the gate passes).
7. Repeat step 5: /list-pricing-plans for M1 now reports nothing, /list-pricing-rules-associated-to-pricing-plan for M1 answers 404 ResourceNotFoundException, and /list-pricing-plans-associated-with-pricing-rule for M1 omits PricingPlanArns — the closed month's record of the plan is gone.
8. Contrast the indirect path the crate states is the same rule: create a billing group, delete it with /delete-billing-group, and read /list-billing-groups for a period before the delete — the group is still reported (`tests/all/deleted_billing_group_period_reads.rs`).
