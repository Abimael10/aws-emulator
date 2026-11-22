Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 0607523cc4a05d467d979a52c3af01f5efd82726

# DeletePricingRule erases the rule from every EARLIER billing period it existed in, unlike the two sibling deletes that end a record only from the period the call is made in

_Filed: 2026-11-22T04:28:40Z_
_Operation: DeletePricingRule | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `0607523cc4a05d467d979a52c3af01f5efd82726`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A pricing rule is a billing-period-scoped record in this crate exactly as a pricing plan and a billing group are: it carries generations (`PricingRuleVersion`), the crate answers "did this rule exist in this billing period" (`pricing_rule_exists_in`), and `ListPricingRules` reports a rule only for the periods it existed in (`rule.exists_in(&billing_period)`).

`DeletePricingRule` applies no billing-period bound: `state.remove_pricing_rule(&rule_arn)` does `self.pricing_rules.remove(rule_arn)`. Since every period-scoped read walks `state.pricing_rules`, the rule disappears from EVERY earlier billing period too:

* `ListPricingRules` with `BillingPeriod` = a month before the delete reported the rule (with its generation's `Name`/`Description`/`ModifierPercentage`/`Tiering`) and now reports nothing;
* `ListPricingPlansAssociatedWithPricingRule` for that earlier month answered 200 (its existence gate is period-scoped, `pricing_rule_exists_in`) and now answers the modelled 404 `ResourceNotFoundException`.

`remove_pricing_rule`'s own doc already names the sibling that does it the other way ("unlike the associations [`Self::remove_custom_line_item`] strips and [`Self::remove_billing_group`] ends at the period of the delete"), and justifies only the ASSOCIATIONS half — nothing in the crate justifies dropping the rule's own record out of the periods a read can still ask about. `remove_billing_group` states the rule in general terms ("like every other billing-period-scoped record a delete that names no range of its own ends, it stops applying FROM the billing period the delete is made in and never earlier"), and the crate's suite restates it for every billing-period-scoped record.

## Evidence

The handler applies no period bound — `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/delete_pricing_rule.rs:207-210`:

        // One state method removes the rule and the index entries that only existed
        // because of it: its name-index entry (so the name can be reused) and the
        // idempotency entries that named it.
        state.remove_pricing_rule(&rule_arn);

and that method removes the record from the map every period-scoped read walks — `src/state.rs:3870-3872`:

    pub fn remove_pricing_rule(&mut self, rule_arn: &str) -> Option<PricingRuleRecord> {
        let removed = self.pricing_rules.remove(rule_arn)?;
        self.pricing_rule_names.remove(&removed.name);

The same method's doc names the sibling that ends records at the period of the delete instead, and justifies only the associations — `src/state.rs:3859-3868`:

    /// The associations pricing PLANS hold to the rule are not ended here, unlike the
    /// associations [`Self::remove_custom_line_item`] strips and
    /// [`Self::remove_billing_group`] ends at the period of the delete:
    /// an association names a resource that exists is enforced for a pricing rule
    /// one step earlier, by refusing the delete outright while any billing period reports
    /// an association to it

A rule IS a billing-period-scoped record — `src/state.rs:3990-4004` (`pricing_rule_exists_in`):

    /// Did the pricing rule `rule_arn` exist in `billing_period` — is it one of the rules
    /// a read scoped to that period reports?
    ...
    /// asked about the ONE rule a read addresses by ARN, so a read of one rule cannot
    /// disagree with the listing of the period it asks about: a rule exists from the
    /// period it was created in on.

and the period-scoped listing reports it on exactly that basis — `src/ops/list_pricing_rules.rs:234-242`:

            .filter(|(arn, rule)| {
                wanted.as_ref().is_none_or(|wanted| wanted.contains(*arn))
                    && rule.exists_in(&billing_period)
            })

while `ListPricingPlansAssociatedWithPricingRule` turns the same question into a 404 — `src/ops/list_pricing_plans_associated_with_pricing_rule.rs:200-211`:

        if !state.pricing_rule_exists_in(&pricing_rule_arn, &billing_period) {
            return Err(
                ListPricingPlansAssociatedWithPricingRuleErrorModeled::ResourceNotFoundException(

The rule the crate states for every such record — `src/state.rs:3544-3549` and `tests/all/deleted_billing_group_keeps_its_period_charges.rs:10-12`:

        // ... like every other billing-period-scoped record a delete that names no
        // range of its own ends, it stops applying FROM the billing period the delete is
        // made in and never earlier ([`BillingPeriodSpan::ending_now`])

    //! RULE: a delete that names no billing-period range ends every billing-period-scoped
    //! record it takes with it FROM the billing period the call is made in and never earlier

Test coverage: `tests/all/pricing_rule_delete_reader_agreement.rs:7-24` pins only that every rule ARN a period-scoped reader of an ASSOCIATION reports still resolves, and `tests/all/pricing_rule_lifecycle.rs` / `tests/all/pricing_rule_billing_periods.rs` never ask what an earlier billing period reports after a `DeletePricingRule`, so nothing pins the retroactive erasure.

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as every test harness in this crate does).
2. POST /create-pricing-rule with {"Name":"Rule1","Scope":"GLOBAL","Type":"MARKUP","ModifierPercentage":10} and keep the ARN R. Associate no pricing plan with it.
3. Backdate the rule's stored `creation_time_millis`/`last_modified_time_millis` by one month (the way `tests/all/pricing_rule_billing_periods.rs` does), so R existed in the PREVIOUS billing period M1.
4. POST /list-pricing-rules with {"BillingPeriod":M1} -> R is reported; POST /list-pricing-plans-associated-with-pricing-rule with {"PricingRuleArn":R,"BillingPeriod":M1} -> HTTP 200.
5. POST /delete-pricing-rule with {"Arn":R} -> HTTP 200 (no plan associates R, so the gate passes).
6. Repeat step 4: /list-pricing-rules for M1 now reports nothing and /list-pricing-plans-associated-with-pricing-rule for M1 answers 404 ResourceNotFoundException — the closed month's record of the rule is gone.
7. Contrast /delete-billing-group on a billing group backdated the same way: a read of a period before that delete still reports the group.
