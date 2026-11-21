Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: df6c59e1d392df4e843198fdd80c29d823f1c724

# A DeleteCustomLineItem naming no BillingPeriodRange erases the item from the CLOSED billing periods it charged, breaking the crate's own "a delete that names no range ends records from the current period and never earlier" rule

_Filed: 2026-11-21T02:47:16Z_
_Operation: DeleteCustomLineItem | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `df6c59e1d392df4e843198fdd80c29d823f1c724`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteCustomLineItem`'s two branches disagree about which billing periods a delete may act in.

The BOUNDED branch reads the operation's own modelled sentence as a window on the periods the delete may take effect in and enforces it (`check_billing_period_range_is_modifiable`): a range starting before the previous billing period is refused because "a month whose bill is already closed" is not a period this operation may act in.

The UNBOUNDED branch (no `BillingPeriodRange`) applies no period bound at all: it calls `state.remove_custom_line_item(&item_arn)`, which deletes the stored record. Because every period-scoped read and the whole charge settlement walk `state.custom_line_items`, the item instantly disappears from EVERY earlier billing period too — months whose bill is closed and that really did carry its charge. `ListCustomLineItems` for such a period reports nothing, `ListCustomLineItemVersions` reports nothing, and `GetBillingGroupCostReport` / `ListBillingGroupCostReports` recompute that closed month's `ProformaCost`, `Margin` and `MarginPercentage` without the charge they previously reported.

That contradicts the rule this crate states and tests for exactly this situation. `remove_billing_group` — the other path that deletes custom line items with no range of its own — ends them at `BillingPeriodSpan::ending_now()` precisely so the closed periods keep reporting and pricing them, and says so while calling the rule general ("like every other billing-period-scoped record a delete that names no range of its own ends"). `BatchDisassociateResourcesFromCustomLineItem`'s no-range branch uses the same `ending_now()` for the same stated reason. The crate's own suite states the rule in universal terms ("a delete that names no billing-period range ends every billing-period-scoped record it takes with it FROM the billing period the call is made in and never earlier").

The net effect is observable retroactive loss of a closed month's charge: deleting the same item indirectly (by deleting its billing group) preserves the closed-period charge, while deleting it directly with no range destroys it.

Note for the fixer: `tests/all/delete_custom_line_item.rs::a_delete_naming_no_billing_period_range_removes_the_item_from_every_period` currently pins the erase, and its stated reason is a reading of the operation doc's first clause alone; the quoted clause "in the current, or previous billing period" — which the same handler enforces as a period window on its other branch — is what contradicts it. Narrowing with `remove_custom_line_item_periods(arn, &BillingPeriodSpan::ending_now(), now)` is the existing helper that already implements the rule (it also narrows the associations keyed on the item and removes the record outright once nothing is left, so an item that applied in no earlier period still ends up gone and a second delete still answers `MISSING_CUSTOMLINEITEM`).

## Evidence

Smithy, the operation's own documentation (`smithy_query(query="doc",shape="DeleteCustomLineItem")`):

    "Deletes the custom line item identified by the given ARN in the current, or previous billing period."

The handler reads that clause as a PERIOD WINDOW on its bounded branch — `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/delete_custom_line_item.rs:204-213`:

                // "Deletes the custom line item … in the current, or previous billing
                // period": the periods this operation may take effect in are bounded
                // to that window, so a range starting outside it — an already closed
                // month or one that has not begun — is refused and the stored item is
                // left exactly as it was.
                if let Err((reason, field, message)) =
                    crate::state::check_billing_period_range_is_modifiable(range)

and applies NO bound at all on its unbounded branch — `src/ops/delete_custom_line_item.rs:186-194`:

            // "Deletes the custom line item identified by the given ARN": a request
            // that names no billing periods deletes the item itself, so no read of any
            // billing period reports it again. One state method removes the item and
            // everything that only existed because of it: the associations other
            // percentage items held to it, and the idempotency entries that named it.
            None => {
                state.remove_custom_line_item(&item_arn);
                "deleted"
            }

`remove_custom_line_item` deletes the record from the map every period-scoped read walks — `src/state.rs:3660-3661`:

    pub fn remove_custom_line_item(&mut self, item_arn: &str) -> Option<CustomLineItemRecord> {
        let removed = self.custom_line_items.remove(item_arn)?;

and the charge settlement for every billing period walks exactly that map — `src/state/charges.rs:36-39`:

        for item in self.custom_line_items.values() {
            if item.billing_group_arn != group_arn || !item.covers(billing_period) {
                continue;
            }

The SAME crate, for the same resource and the same "a delete that names no range" case, does the opposite — `src/state.rs:3544-3554` and `3566` (`remove_billing_group`):

        // A custom line item "applies to" one billing group, so it cannot outlive it —
        // and, like every other billing-period-scoped record a delete that names no
        // range of its own ends, it stops applying FROM the billing period the delete is
        // made in and never earlier ([`BillingPeriodSpan::ending_now`]): the periods
        // before it are closed bills the item really carried its charge in, and they go
        // on reporting the group ([`Self::billing_groups_in`]), so they go on reporting
        // the item and pricing it.
        let ended = BillingPeriodSpan::ending_now();
    ...
            if self.remove_custom_line_item_periods(arn, &ended, now)

The sibling no-range writer applies the same rule — `src/ops/batch_disassociate_resources_from_custom_line_item.rs:320-325`:

            // A request that names no range ENDS the association at the billing period
            // this call is made in: it stays in effect in every earlier period, so a
            // read scoped to one of those closed bills still reports the resource, and
            // in none from this period on
            // ([`crate::state::BillingPeriodSpan::ending_now`]).
            None => crate::state::BillingPeriodSpan::ending_now(),

The crate's own suite states the rule as general, naming the custom line items and their charges — `tests/all/deleted_billing_group_keeps_its_period_charges.rs:10-13`:

    //! RULE: a delete that names no billing-period range ends every billing-period-scoped
    //! record it takes with it FROM the billing period the call is made in and never earlier,
    //! so a read scoped to a period before the delete reports and prices the group's custom
    //! line items and their resource associations exactly as it did before the delete

Test coverage: the contradicting behaviour is pinned by `tests/all/delete_custom_line_item.rs:332-336` and `:398-404`, whose stated reason is "THE RULE for a request that names no billing periods: the item itself is deleted, so no read of any billing period reports it again" — a reading of the operation doc's first clause that drops the "in the current, or previous billing period" clause the same handler enforces at `:204-213`. No existing test covers a closed-period cost report after an unbounded `DeleteCustomLineItem` (`tests/all/deleted_billing_group_keeps_its_period_charges.rs` only drives the `DeleteBillingGroup` path).

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as every test harness in this crate does: `engine.state::<...State>().lock().payer_account_id = Some(emulator_account_id())`).
2. POST /create-billing-group with `ComputationPreference.PricingPlanArn = arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan`; keep the group ARN G.
3. POST /create-custom-line-item with `Name="Fee"`, `BillingGroupArn=G`, `ChargeDetails={Type:FEE, Flat:{ChargeValue:100}}` and `BillingPeriodRange={InclusiveStartBillingPeriod:<previous billing period>}` (the previous period is inside the window the handler allows); keep the ARN C.
4. POST /get-billing-group-cost-report with `{Arn:G, BillingPeriodRange:{InclusiveStartBillingPeriod:<previous>, ExclusiveEndBillingPeriod:<current>}}` -> `ProformaCost` is 100, and POST /list-custom-line-items with `{BillingPeriod:<previous>, Filters:{Arns:[C]}}` reports C.
5. POST /delete-custom-line-item with `{"Arn":C}` — NO `BillingPeriodRange` -> HTTP 200.
6. Repeat step 4: /get-billing-group-cost-report for the PREVIOUS (closed) billing period now reports `ProformaCost` 0, `Margin` 0 and `MarginPercentage` 0, and /list-custom-line-items for that period reports nothing — the closed month's charge is gone.
7. Contrast the indirect path, which the crate states is the same rule: redo steps 2-4, then POST /delete-billing-group with `{"Arn":G}` and repeat step 4 — the previous period still reports and prices C (`tests/all/deleted_billing_group_keeps_its_period_charges.rs`).
