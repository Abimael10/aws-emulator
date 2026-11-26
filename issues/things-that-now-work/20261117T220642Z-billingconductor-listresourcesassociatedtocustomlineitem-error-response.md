Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: e311c11a5225a75aa165947dc04df865f5f68358

# The custom-line-item existence gate is not billing-period scoped, so a period that held no version of the item is answered 200 instead of the modelled 404

_Filed: 2026-11-17T22:06:42Z_
_Operation: ListResourcesAssociatedToCustomLineItem | Class: error-response_

> _Note: this issue was identified against the code base at commit `e311c11a5225a75aa165947dc04df865f5f68358`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListResourcesAssociatedToCustomLineItem` is scoped to ONE billing period (`BillingPeriod`: "The billing period for which the resource associations will be listed", defaulted to the current one at line 207-209) and models `ResourceNotFoundException` (HTTP 404). Its existence gate, however, asks the LIVE store only: `state.custom_line_items.get(&arn)`. So a read scoped to a billing period in which NO generation of the item applies — a period before the item was created, or a period a bounded `DeleteCustomLineItem` has narrowed the item out of — is answered HTTP 200 with an empty/partial `AssociatedResources`, while `ListCustomLineItems` for that very period reports no such item at all (it filters on `item.effective_in(&billing_period)`).

The crate's two OTHER cross-reference listings, which have the identical shape (one parent resource addressed by ARN + a `BillingPeriod` + a modelled 404), both gate the parent's existence IN THE PERIOD BEING READ and answer the modelled 404 for a period that held no such parent:
- `ListPricingPlansAssociatedWithPricingRule` calls `state.pricing_rule_exists_in(&pricing_rule_arn, &billing_period)`.
- `ListPricingRulesAssociatedToPricingPlan` gets `None` from `associated_pricing_rule_arns`, which first asks `pricing_plan_exists_in(plan_arn, billing_period)`.

The rule those two implement is stated in this crate's own state module, on `pricing_rule_exists_in`: a read of one resource "cannot disagree with the listing of the period it asks about". The custom-line-item reader is the one member of the family that does disagree with its own period-scoped listing.

The same divergence shows up in the opposite direction inside this crate: `remove_custom_line_item_periods` states that "a period no generation of the item applies in holds no association of it either" and strips the associations for exactly those periods — yet the reader still answers 200 for such a period rather than reporting that the period held no such item.

## Evidence

Handler, gate on the LIVE store only — `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/list_resources_associated_to_custom_line_item.rs:183-195`:

        // The `CustomLineItemArn` pattern accepts the full ARN or the bare id.
        let arn = crate::state::normalize_custom_line_item_arn(&input.arn);
        let Some(item) = state.custom_line_items.get(&arn) else {
            return Err(
                ListResourcesAssociatedToCustomLineItemErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: "the custom line item does not exist".to_string(),

and the period the read is scoped to, decided AFTER that gate — same file `:207-209`:

        let billing_period = input
            .billing_period
            .unwrap_or_else(crate::state::current_billing_period);

Sibling 1, period-scoped gate — `src/ops/list_pricing_plans_associated_with_pricing_rule.rs:200-211`:

        if !state.pricing_rule_exists_in(&pricing_rule_arn, &billing_period) {
            return Err(
                ListPricingPlansAssociatedWithPricingRuleErrorModeled::ResourceNotFoundException(

Sibling 2, same gate through the state method — `src/ops/list_pricing_rules_associated_to_pricing_plan.rs:202-217`:

        let Some(rule_arns) = state.associated_pricing_rule_arns(&pricing_plan_arn, &billing_period)
        else {
            // The plan is the resource this read references, and whether it exists is
            // asked of the period being read: a period this account held no such plan in
            // is the modelled 404 rather than a 200 reporting no association.

and `src/state.rs:4044-4046`:

        if !self.pricing_plan_exists_in(plan_arn, billing_period) {
            return None;
        }

The rule the crate states for exactly this question — `src/state.rs:3892-3903` (doc of `pricing_rule_exists_in`):

    /// The same question the period-scoped LISTING of the rules answers for a whole
    /// period ([`VersionedResource::exists_in`], which `ListPricingRules` filters on),
    /// asked about the ONE rule a read addresses by ARN, so a read of one rule cannot
    /// disagree with the listing of the period it asks about

The period-scoped listing this reader disagrees with — `src/ops/list_custom_line_items.rs:296-302`:

        for (arn, item) in &state.custom_line_items {
            // The version of the item in effect in the billing period asked about is
            // what this read reports; an item no version of which applies in that period
            // is not reported at all.
            let Some(version) = item.effective_in(&billing_period) else {
                continue;
            };

and the write-side statement of the same invariant — `src/ops/delete_custom_line_item.rs:222-227`:

                // One state method takes those periods out of every generation of the
                // item AND out of every resource association keyed on it — ... because a
                // period no generation of the item applies in holds no association of it
                // either

Model: `smithy_query(query="operation",operation="ListResourcesAssociatedToCustomLineItem")` declares `ResourceNotFoundException` (`httpError` 404) for this operation and documents `BillingPeriod` as "The billing period for which the resource associations will be listed".

Test coverage: `tests/all/resource_association_billing_periods.rs` drives this read for periods outside the item's own range, but its helper asserts the status unconditionally — `tests/all/resource_association_billing_periods.rs:200-203`:

        assert_eq!(
            status, 200,
            "case {case:?}: expected the read to answer HTTP 200, got {status} with {answered}"
        );

so the row `case: "a period long before this account held any custom line item", billing_period: "2020-01"` (`:498-504`) currently pins the 200. That row's stated rule is about WHAT is reported ("No billing period BEFORE an association is made reports it", `:491-494`), not about the status, so the status expectation for the pre-existence periods is what a fix updates; the "does not report it" rule is preserved by a 404. No existing issue names this operation's error response (the only filed issue for it, `20261024T181634Z-...-listresourcesassociatedtocustomlineitem-wrong-behavior.md`, is about the association period filter, class `wrong-behavior`).

## Steps to Reproduce

1. Designate the emulated account as the payer so the control plane is reachable (set `ComAmazonawsBillingconductorAwsbillingconductorState::payer_account_id = Some(engine::emulator_account_id())`, as every suite in this crate does).
2. POST /create-billing-group with ComputationPreference.PricingPlanArn = arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan -> group G.
3. POST /create-custom-line-item against G with ChargeDetails {"Type":"FEE","Percentage":{"PercentageValue":10}} and no BillingPeriodRange -> item I, which applies from the CURRENT billing period M on.
4. PUT /batch-associate-resources-to-custom-line-item with {"TargetArn": I, "ResourceArns": [G]} and no BillingPeriodRange.
5. POST /list-custom-line-items with {"BillingPeriod":"2020-01"} -> no item is reported: no version of I applies in that period.
6. POST /list-resources-associated-to-custom-line-item with {"Arn": I, "BillingPeriod":"2020-01"}. Observed: HTTP 200 with an empty AssociatedResources. Expected (the answer the two sibling cross-reference listings give for a period their parent did not exist in): the modelled 404 ResourceNotFoundException with ResourceType "customlineitem".
7. Contrast within the same emulator: POST /list-pricing-plans-associated-with-pricing-rule for a rule created now but with {"BillingPeriod":"2020-01"} -> HTTP 404, and POST /list-pricing-rules-associated-to-pricing-plan for a plan created now with the same period -> HTTP 404.
8. Second symptom, after a bounded narrowing: POST /delete-custom-line-item with {"Arn": I, "BillingPeriodRange":{"InclusiveStartBillingPeriod": M}} (I is narrowed out of M and every later period but still stored if it applied earlier). POST /list-custom-line-items with {"BillingPeriod": M} reports no item, while POST /list-resources-associated-to-custom-line-item with {"Arn": I, "BillingPeriod": M} still answers HTTP 200.
