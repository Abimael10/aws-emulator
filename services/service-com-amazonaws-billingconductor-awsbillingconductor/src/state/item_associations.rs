// SPDX-License-Identifier: CC0-1.0

//! Keeping the resource associations KEYED ON a custom line item inside the billing
//! periods the item applies in — what every write that takes billing periods away from an
//! item owes them.
//!
//! One concern, so the two writers that can take billing periods away from a stored item —
//! the bounded delete
//! ([`ComAmazonawsBillingconductorAwsbillingconductorState::remove_custom_line_item_periods`])
//! and the write that supersedes a generation over a bounded range
//! ([`ComAmazonawsBillingconductorAwsbillingconductorState::put_custom_line_item`]) — do
//! the same thing to the associations keyed on the item, rather than one of them leaving a
//! period reporting an association to an item it reports no version of.

use super::*;

/// The billing periods `before` covers and `after` does not: what a write TOOK AWAY from
/// the stretches a resource applies over, as spans in ascending order.
///
/// Each stretch keeps only what no stretch the write left covers
/// ([`BillingPeriodSpan::minus`], applied once per stretch left), so a write that only
/// widened the stretches takes nothing away and one that narrowed them away at one end
/// leaves exactly the periods beyond that end. The lists on either side are disjoint and
/// ascending ([`CustomLineItemRecord::applied_periods`]), so what is left is too.
fn billing_periods_lost(
    before: &[BillingPeriodSpan],
    after: &[BillingPeriodSpan],
) -> Vec<BillingPeriodSpan> {
    let mut lost = before.to_vec();
    for kept in after {
        lost = lost.iter().flat_map(|span| span.minus(kept)).collect();
    }
    lost
}

/// Take every stretch of `removed` OUT of the associations of `associations` that
/// `narrowed` picks out, and report what that did to them as a whole.
///
/// Each stretch comes out through the narrowing every store here shares
/// ([`remove_association_periods`]), so an association keeps whatever of itself lies
/// outside them all and one left in effect in no billing period is dropped. The whole
/// narrowing is REFUSED at the [`MAX_BILLING_PERIOD_SPANS`] bound and nothing of the list
/// is written until every stretch is through, exactly as the addition of several stretches
/// at once is all-or-nothing ([`add_billing_period_spans`]) — of which this is the inverse,
/// for the caller that has worked out the STRETCHES a write took away from a resource
/// rather than one range it was asked to remove.
fn remove_association_period_spans(
    associations: &mut Vec<ResourceAssociationRecord>,
    removed: &[BillingPeriodSpan],
    narrowed: impl Fn(&ResourceAssociationRecord) -> bool,
) -> PeriodsRemoval {
    let mut kept = associations.clone();
    let mut outcome = PeriodsRemoval::Unchanged;
    for span in removed {
        match remove_association_periods(&mut kept, span, &narrowed) {
            PeriodsRemoval::Changed => outcome = PeriodsRemoval::Changed,
            PeriodsRemoval::Unchanged => {}
            PeriodsRemoval::AtSpanLimit => return PeriodsRemoval::AtSpanLimit,
        }
    }
    if outcome == PeriodsRemoval::Changed {
        *associations = kept;
    }
    outcome
}

impl ComAmazonawsBillingconductorAwsbillingconductorState {
    /// Take the billing periods the stretches of `removed` name out of every resource
    /// association KEYED ON the custom line item `item_arn`: the ones the item itself
    /// holds — `item`, the record the caller is writing, which may not be the one the
    /// store still holds — and the ones other percentage items hold TO it.
    ///
    /// A resource association is in effect only in billing periods the resource it names
    /// is there to be associated in, so a period the item applies in in no generation of
    /// it holds no association of it either: the ones the item holds, because the item is
    /// not there to hold a resource associated in a period it does not apply in, and the
    /// ones other items hold to it, because there is no version of it for them to be a
    /// share of. An association left in effect in no period at all goes
    /// ([`remove_association_periods`]).
    ///
    /// Nothing is written unless the WHOLE narrowing stays inside
    /// [`MAX_BILLING_PERIOD_SPANS`] — every association keyed on the item, over every
    /// stretch — so a narrowing refused there leaves the store, and `item`, exactly as
    /// they were. ONE call does every stretch at once, so a write narrows the store once
    /// however many stretches it took away, and a `removed` naming none walks nothing.
    pub(super) fn take_item_association_periods(
        &mut self,
        item_arn: &str,
        item: &mut CustomLineItemRecord,
        removed: &[BillingPeriodSpan],
    ) -> PeriodsRemoval {
        if removed.is_empty() {
            return PeriodsRemoval::Unchanged;
        }
        // Every narrowing below lands on a COPY of a record, so one refused at the span
        // bound can be answered without having written a thing.
        let mut held = item.associated_resources.clone();
        let mut outcome = match remove_association_period_spans(&mut held, removed, |_| true) {
            PeriodsRemoval::AtSpanLimit => return PeriodsRemoval::AtSpanLimit,
            outcome => outcome,
        };
        let mut holders: Vec<(String, CustomLineItemRecord)> = Vec::new();
        for (arn, holder) in &self.custom_line_items {
            if arn == item_arn
                || !holder
                    .associated_resources
                    .iter()
                    .any(|association| association.arn == item_arn)
            {
                continue;
            }
            let mut holder = holder.clone();
            match remove_association_period_spans(
                &mut holder.associated_resources,
                removed,
                |association| association.arn == item_arn,
            ) {
                // Only a holder whose associations really did lose a billing period is
                // written back: a holder whose association the stretches miss entirely is
                // the record the store already holds.
                PeriodsRemoval::Changed => {
                    outcome = PeriodsRemoval::Changed;
                    holders.push((arn.clone(), holder));
                }
                PeriodsRemoval::Unchanged => {}
                PeriodsRemoval::AtSpanLimit => return PeriodsRemoval::AtSpanLimit,
            }
        }

        item.associated_resources = held;
        for (arn, holder) in holders {
            self.custom_line_items.insert(arn, holder);
        }
        outcome
    }

    /// Take out of every resource association keyed on the custom line item `item_arn`
    /// the billing periods a write just took away from the item: the ones it applied in
    /// before the write (`applied_before`) and applies in in no generation after it
    /// (`applied_after`).
    ///
    /// The rule [`Self::take_item_association_periods`] states, asked of a write that
    /// states the periods a generation applies in rather than naming the ones to remove:
    /// whatever such a write leaves the item applying in nowhere is taken away from the
    /// item as surely as a bounded delete takes it, so it comes out of the associations
    /// keyed on the item just the same. A write that left the item applying in every
    /// period it already did — one that only widened the stretches, or restated them —
    /// takes nothing out of any association and so reads no other record at all.
    pub(super) fn end_associations_outside_item_periods(
        &mut self,
        item_arn: &str,
        item: &mut CustomLineItemRecord,
        applied_before: &[BillingPeriodSpan],
        applied_after: &[BillingPeriodSpan],
    ) {
        let lost = billing_periods_lost(applied_before, applied_after);
        let outcome = self.take_item_association_periods(item_arn, item, &lost);
        // The periods lost are periods the item applied in, and an association keyed on it
        // is in effect in none but those, so taking them out shortens each association's
        // stretches and splits none of them: the span bound cannot be reached from here.
        debug_assert!(
            outcome != PeriodsRemoval::AtSpanLimit,
            "narrowing the associations keyed on {item_arn} to the billing periods it still \
             applies in cannot leave one in more stretches than it already held"
        );
        // It is still the one outcome that writes nothing, so a store that ever did reach
        // it says so rather than passing a narrowing it did not make off as made.
        if outcome == PeriodsRemoval::AtSpanLimit {
            ::tracing::warn!(
                custom_line_item_arn = %item_arn,
                cap = MAX_BILLING_PERIOD_SPANS,
                "an association keyed on a custom line item kept billing periods the item \
                 lost, at the store's span cap"
            );
        }
    }
}
