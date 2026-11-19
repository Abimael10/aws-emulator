Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 08a32c2b45cf1544e9455c75c90bbd9565d142c8

# A bounded UpdateCustomLineItem range takes billing periods away from the item without narrowing a single resource association keyed on it

_Filed: 2026-11-19T20:19:53Z_
_Operation: UpdateCustomLineItem | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `08a32c2b45cf1544e9455c75c90bbd9565d142c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateCustomLineItem` carrying a `BillingPeriodRange` bounded on BOTH sides replaces the item's spans with exactly that range, and `put_custom_line_item` then takes `[InclusiveStartBillingPeriod, infinity)` away from every other generation. The item therefore applies in NO billing period from the range's `ExclusiveEndBillingPeriod` on — the crate's own test pins exactly that (`tests/all/update_custom_line_item.rs:691-706` lists `period(3)` and `period(16)` among "the periods that must report no item at all" for a range ending at `period(3)`).

What the write does NOT do is narrow the resource associations keyed on that item: neither the associations the item itself holds, nor the associations other percentage custom line items hold TO it. `put_custom_line_item` only rewrites `spans`/`superseded_versions` and re-inserts the record; it never calls `remove_association_periods`, which is the helper the crate's OTHER narrowing writer (`remove_custom_line_item_periods`, the bounded `DeleteCustomLineItem` path) calls for exactly this purpose.

The result is stored state the crate declares impossible: a billing period that reports no version of the child item still reports an association to it. A parent percentage item in the same group answers `ListResourcesAssociatedToCustomLineItem` for such a period with the child in `AssociatedResources`, and `ListCustomLineItems` counts the child in the parent's `AssociationSize` (both read `ResourceAssociationRecord::covers`, which still covers those periods), while `ListCustomLineItems` for that same period reports no child item at all. The item's own associations survive the same way.

This is the narrowing side of the invariant the crate states three times and devotes a whole test suite to; the already-open `BatchAssociateResourcesToCustomLineItem` issue is the writing side of the same invariant, not this path.

## Evidence

The rule, as the crate's own test suite states it for EVERY write — `tests/all/bounded_delete_narrows_resource_associations.rs:9-15`:

    //! RULE: a resource association is in effect only in billing periods the resource it
    //! names applies in, so a write that takes billing periods away from a custom line item
    //! takes the same periods out of every association keyed on that item — the ones the item
    //! itself holds and the ones other percentage items hold TO it — and drops an association
    //! left in effect in no period at all. So no read of a billing period reports an
    //! association to an item no version of which that period reports

The same rule in the state module — `src/state.rs:3621-3630` (doc of `remove_custom_line_item_periods`):

    /// A resource association is in effect only in billing periods the resource it names
    /// is there to be associated in, so narrowing the periods a resource applies in
    /// narrows by the same periods every association keyed on that resource — the ones
    /// the resource itself holds and the ones other resources hold TO it — and an
    /// association left in effect in no period at all goes
    /// ([`remove_association_periods`]). ... a period the item applies in in no generation
    /// of it holds no association of it either, so no read of that period can report an
    /// association to an item it reports no version of.

and again at `src/state.rs:4032-4036` (doc of `associable_billing_periods`):

    /// A resource association is in effect only in billing periods the resource it names
    /// is there to be associated in — the rule the narrowing writer maintains from the
    /// other end ([`Self::remove_custom_line_item_periods`]): a period the named resource
    /// applies in in no generation of it holds no association of it either, so no read of
    /// that period can report an association to an item it reports no version of.

The path that HONOURS it (bounded delete) — `src/state.rs:3659` and `src/state.rs:3676-3680`:

        if remove_association_periods(&mut item.associated_resources, removed, |_| true)
            == PeriodsRemoval::AtSpanLimit
    ...
            let outcome = remove_association_periods(
                &mut holder.associated_resources,
                removed,
                |association| association.arn == item_arn,
            );

The path that BREAKS it (bounded update) — `src/ops/update_custom_line_item.rs:377-380` replaces the item's spans with exactly the request's range:

        updated.spans = vec![crate::state::BillingPeriodSpan::new(
            Some(range.inclusive_start_billing_period.clone()),
            range.exclusive_end_billing_period.clone(),
        )];

`src/ops/update_custom_line_item.rs:255` hands that to the writer:

            state.put_custom_line_item(updated, now, takes_effect_from.as_deref());

and `put_custom_line_item` (`src/state.rs:3724-3770`) takes the periods away from every generation — `src/state.rs:3743-3747`:

                Some(applied_from) => {
                    let taken = BillingPeriodSpan::new(Some(applied_from.to_string()), None);
                    for generation in kept.iter_mut().chain(std::iter::once(&mut replaced)) {
                        remove_billing_periods(&mut generation.spans, &taken);
                    }

and then simply re-inserts the record (`src/state.rs:3769`: `self.custom_line_items.insert(arn, updated);`). `remove_association_periods` is never called on this path (`grep remove_association_periods src/` matches only `state.rs`'s delete path), and `merge` clones the stored record's `associated_resources` untouched (`src/ops/update_custom_line_item.rs:285`: `let mut updated = stored.clone();`).

That the update really does take those periods away is pinned by `tests/all/update_custom_line_item.rs:691-706`, row "a half-open range of three months": range `[period(0), period(3))`, and `vec![period(-2), period(3), period(16)]` are "the periods that must report no item at all".

Test coverage: `tests/all/bounded_delete_narrows_resource_associations.rs` drives the rule only through `DeleteCustomLineItem`; `tests/all/association_size_is_billing_period_scoped.rs:492-508` updates the PARENT item, never a narrowed child, so no test covers an `UpdateCustomLineItem` that narrows an item another item is associated to.

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as the test harnesses do: `engine.state::<BillingConductorState>().lock().payer_account_id = Some(emulator_account_id())`).
2. POST /create-billing-group with `ComputationPreference.PricingPlanArn = arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan`; keep the group ARN G.
3. POST /create-custom-line-item with `Name="Child"`, `BillingGroupArn=G`, `ChargeDetails={Type:FEE, Flat:{ChargeValue:10}}` and NO BillingPeriodRange; keep the ARN C (it applies from the current billing period on, open-ended).
4. POST /create-custom-line-item with `Name="Parent"`, `BillingGroupArn=G`, `ChargeDetails={Type:FEE, Percentage:{PercentageValue:50, AssociatedValues:[C]}}` and no range; keep the ARN P.
5. POST /list-resources-associated-to-custom-line-item with `{Arn:P, BillingPeriod:<current+1>}` -> C is reported (the association is open-ended).
6. POST /update-custom-line-item with `{Arn:C, Description:"narrowed", BillingPeriodRange:{InclusiveStartBillingPeriod:<current>, ExclusiveEndBillingPeriod:<current+1>}}` -> HTTP 200.
7. POST /list-custom-line-items with `{BillingPeriod:<current+1>, Filters:{Arns:[C]}}` -> EMPTY: the period reports no version of C.
8. POST /list-resources-associated-to-custom-line-item with `{Arn:P, BillingPeriod:<current+1>}` -> C is STILL reported, and POST /list-custom-line-items with `{BillingPeriod:<current+1>, Filters:{Arns:[P]}}` reports `AssociationSize: 1` — the period reports an association to an item it reports no version of, which the same range given to /delete-custom-line-item would have narrowed away.
