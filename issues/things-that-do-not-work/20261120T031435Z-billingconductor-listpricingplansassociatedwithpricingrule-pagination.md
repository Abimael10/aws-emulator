Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 48b4999bcfc6fe53ee977f3df6a77f3d1baf018d

# The plan-association NextToken is not scoped to the BillingPeriod that minted it, so another period reads it as a cut point

_Filed: 2026-11-20T03:14:35Z_
_Operation: ListPricingPlansAssociatedWithPricingRule | Class: pagination_

> _Note: this issue was identified against the code base at commit `48b4999bcfc6fe53ee977f3df6a77f3d1baf018d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This read is answered FOR ONE billing period: `BillingPeriod` (defaulting to the current month) decides which plans associated the rule then, so the sequence of plan ARNs paged over differs per month. The handler nevertheless mints and reads its `NextToken` under `page_scope_within(Self::NAME, &pricing_rule_arn)` — the operation and the rule only, with no month in it — which the crate's own pagination module states is the wrong scope for a read answered for one billing period.

Since `decode_next_token` compares only the scope, a token minted while paging one month decodes under another month's request and is read as a cut point into that other month's sequence: the plan ARNs of the second period sorting below the cursor are silently dropped from the answer, with no `NextToken` to report them when the remainder fits in one page. The crate says such a token must instead be "refused with [`invalid_next_token`] ... rather than read as a cut point into a keyspace it names nothing in".

The period-scoped listings of this crate (`ListBillingGroups`, `ListPricingPlans`, `ListPricingRules`, `ListCustomLineItems`, `ListAccountAssociations`, `ListBillingGroupCostReports`, `ListResourcesAssociatedToCustomLineItem`) all pass their month over; this operation and its mirror `ListPricingRulesAssociatedToPricingPlan` are the two that do not.

## Evidence

The crate's rule — `src/state/pagination.rs:117-130`:

    /// The scope of the tokens a listing read WITHIN one resource mints: the operation and
    /// that resource.
    ...
    /// A read
    /// ANSWERED FOR one billing period is scoped by [`page_scope_in_period`] instead, because the
    /// month it is answered for decides what each reported element is and so which sequence the
    /// read walks.
    pub fn page_scope_within(operation: &str, resource_arn: &str) -> String {

The handler is answered for one month, and the month decides the whole element set — `src/ops/list_pricing_plans_associated_with_pricing_rule.rs:183-190` and `:213-218`:

        // "The pricing plan billing period for which associations will be listed":
        // the read is scoped to ONE period, and an association is in force only from
        // the period it was made in onward. ...
        let billing_period = input
            .billing_period
            .clone()
            .unwrap_or_else(crate::state::current_billing_period);
    ...
        let plan_arns = state.pricing_plans_referencing_rule(&pricing_rule_arn, &billing_period);

yet it pages under a monthless scope — `src/ops/list_pricing_plans_associated_with_pricing_rule.rs:225-231`:

        let (page, next_token) = crate::state::paginate(
            &crate::state::page_scope_within(Self::NAME, &pricing_rule_arn),
            keyed,
            input.next_token.as_deref(),
            input.max_results,
        )

The sibling period-scoped listing of the same resource class passes the month — `src/ops/list_pricing_plans.rs:240-241`:

        let (page, next_token) = crate::state::paginate(
            &crate::state::page_scope_in_period(Self::NAME, None, &billing_period),

and a token is kept on the scope comparison alone — `src/state/pagination.rs:100-105`:

        let (minted_for, cursor) = decoded.split_once(PAGE_TOKEN_SEPARATOR)?;
        if minted_for != scope {
            return None;
        }

Test coverage: `tests/all/pagination_token_opacity.rs:362-369` drives this listing with `body: json!({ "PricingRuleArn": rules[0] })` and an `other_parent` naming a different RULE — cross-parent only. Nothing pins cross-period acceptance.

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as the suites do: `engine.state::<BillingConductorState>().lock().payer_account_id = Some(emulator_account_id())`).
2. POST /create-pricing-rule (rule R), POST /create-pricing-plan twice (plans P1, P2) and POST /associate-pricing-rules so both plans associate R in the previous billing period; then in the current billing period associate R to a third plan P3 as well.
3. POST /list-pricing-plans-associated-with-pricing-rule with `{"PricingRuleArn": R, "BillingPeriod": "<previous billing period>", "MaxResults": 1}` -> a page of 1 and a NextToken.
4. POST /list-pricing-plans-associated-with-pricing-rule with `{"PricingRuleArn": R, "BillingPeriod": "<current billing period>", "NextToken": "<the token from step 3>"}`.
5. Observed: HTTP 200 with the CURRENT period's plan sequence cut at the previous period's cursor — the plan ARNs sorting below it are omitted and no NextToken reports them. Expected per `src/state/pagination.rs:124-127`: the modelled 400 ValidationException with Reason CANNOT_PARSE and Fields naming NextToken.
