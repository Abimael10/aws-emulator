Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 48b4999bcfc6fe53ee977f3df6a77f3d1baf018d

# The rule-association NextToken is not scoped to the BillingPeriod that minted it, so another period reads it as a cut point

_Filed: 2026-11-20T03:14:35Z_
_Operation: ListPricingRulesAssociatedToPricingPlan | Class: pagination_

> _Note: this issue was identified against the code base at commit `48b4999bcfc6fe53ee977f3df6a77f3d1baf018d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This read is answered FOR ONE billing period: `BillingPeriod` (defaulting to the current month) is what selects the rule associations in force, so the sequence of rule ARNs it pages over is a different sequence for every month. The handler nevertheless mints and reads its `NextToken` under `page_scope_within(Self::NAME, &pricing_plan_arn)` — the operation and the plan only, with no month in it — which the crate's own pagination module states is the WRONG scope for a period-scoped read: "A read ANSWERED FOR one billing period is scoped by [`page_scope_in_period`] instead, because the month it is answered for decides what each reported element is and so which sequence the read walks."

Because `decode_next_token` compares only the scope, a token minted while paging one month decodes under another month's request and is read as a cut point into that other month's sequence: every rule ARN of the second period that sorts below the cursor is silently skipped, and if the remainder fits in one page the caller gets no `NextToken` and cannot tell it never saw them. The crate states the opposite outcome for such a token — it is to be "refused with [`invalid_next_token`] ... rather than read as a cut point into a keyspace it names nothing in".

Every other period-scoped listing of this crate already passes the month over, including the structurally identical case of a read WITHIN a parent resource (`ListResourcesAssociatedToCustomLineItem`, which passes `Some(&item.arn)` plus the period). This operation and its mirror `ListPricingPlansAssociatedWithPricingRule` are the two that do not.

## Evidence

The crate's rule for which scope a period-scoped read mints under — `src/state/pagination.rs:117-130`:

    /// The scope of the tokens a listing read WITHIN one resource mints: the operation and
    /// that resource.
    ...
    /// which elements of the one sequence are reported, not which sequence is being read. A read
    /// ANSWERED FOR one billing period is scoped by [`page_scope_in_period`] instead, because the
    /// month it is answered for decides what each reported element is and so which sequence the
    /// read walks.
    pub fn page_scope_within(operation: &str, resource_arn: &str) -> String {

and `src/state/pagination.rs:159-163`, which states that a parent resource and the month travel TOGETHER for such a read:

    /// `resource_arn` is the parent the read is within for the reads whose sequence belongs to
    /// one, and absent for a read of the account's own collection, exactly as
    /// [`page_scope_within`] separates the two.

The handler is answered for one month — `src/ops/list_pricing_rules_associated_to_pricing_plan.rs:193-202`:

        // "The billing period for which the pricing rule associations are to be
        // listed": the read is scoped to ONE period, and an association is in force
        // only from the period it was made in onward ...
        let billing_period = input
            .billing_period
            .clone()
            .unwrap_or_else(crate::state::current_billing_period);

        let Some(rule_arns) = state.associated_pricing_rule_arns(&pricing_plan_arn, &billing_period)

yet pages under a scope with no month in it — `src/ops/list_pricing_rules_associated_to_pricing_plan.rs:224-230`:

        let (page, next_token) = crate::state::paginate(
            &crate::state::page_scope_within(Self::NAME, &pricing_plan_arn),
            keyed,
            input.next_token.as_deref(),
            input.max_results,
        )

The sibling read within a parent AND answered for a period does it the documented way — `src/ops/list_resources_associated_to_custom_line_item.rs:245-246`:

        let (associated_resources, next_token) = crate::state::paginate(
            &crate::state::page_scope_in_period(Self::NAME, Some(&item.arn), &billing_period),

and only the scope is compared when a token is read back — `src/state/pagination.rs:100-105`:

        let (minted_for, cursor) = decoded.split_once(PAGE_TOKEN_SEPARATOR)?;
        if minted_for != scope {
            return None;
        }

Test coverage: `tests/all/pagination_token_opacity.rs:354-361` drives this listing with `body: json!({ "PricingPlanArn": plans[0] })` and an `other_parent` of a DIFFERENT plan only — it pins cross-parent refusal, never cross-period. No test pins the current cross-period acceptance.

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as the suites do: `engine.state::<BillingConductorState>().lock().payer_account_id = Some(emulator_account_id())`).
2. POST /create-pricing-rule twice (rules R1, R2) and POST /create-pricing-plan, then POST /associate-pricing-rules with `{"Arn": plan, "PricingRuleArns": [R1, R2]}` so both associations are in force in the previous billing period, and advance to the next billing period (or arrange R1/R2 to be associated in the earlier period and a third rule R3 associated only in the current one).
3. POST /list-pricing-rules-associated-to-pricing-plan with `{"PricingPlanArn": plan, "BillingPeriod": "<previous billing period>", "MaxResults": 1}` -> a page of 1 and a NextToken.
4. POST /list-pricing-rules-associated-to-pricing-plan with `{"PricingPlanArn": plan, "BillingPeriod": "<current billing period>", "NextToken": "<the token from step 3>"}`.
5. Observed: HTTP 200 with the CURRENT period's association sequence cut at the previous period's cursor — the rule ARNs sorting below that cursor are omitted and no NextToken reports them. Expected per `src/state/pagination.rs:124-127`: the modelled 400 ValidationException with Reason CANNOT_PARSE and Fields naming NextToken, because the token was not minted for the sequence being read.
