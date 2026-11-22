// SPDX-License-Identifier: CC0-1.0

//! The bounded HISTORY behind a half-open billing-period record — one rule for every
//! store here that keeps the stretches a thing was in force over, so that a read of a
//! closed billing period can still report it.
//!
//! Every such record carries the period it began in and, once something ended it, the
//! first period it no longer holds, read half-open ([`billing_period_range_covers`]).
//! The two stores that keep a HISTORY of them — a pricing plan's rule associations
//! ([`PricingRuleAssociationRecord`]) and an account's billing group memberships
//! ([`AccountAssociationRecord`]) — answer the same questions of a record and are
//! pruned the same way, here, so neither can drift from the other:
//!
//! * a record whose range covers no billing period at all is in force in none and
//!   reportable for none, so it is DROPPED rather than kept as a span covering nothing
//!   — which is also what keeps a caller cycling one resource inside ONE billing period
//!   from growing the record at all; and
//! * only the ENDED records count against the bound, and the OLDEST of them goes first,
//!   so a forgotten one reads back the way its period read before anybody recorded it,
//!   and the record still in force is never the one evicted.

use super::*;

/// One record of the stretch of billing periods something was in force over: the
/// half-open range [`Self::inclusive_start_billing_period`] ..
/// [`Self::exclusive_end_billing_period`].
///
/// Implemented by every record a bounded history is kept of, so the questions a read
/// and the pruning ask of such a record — does it still stand, is it in force in a
/// given period, is it in force in any period at all — are answered in ONE place
/// rather than once per record type.
pub trait BillingPeriodHistoryRecord {
    /// The first billing period this record is in force for.
    fn inclusive_start_billing_period(&self) -> &str;

    /// The first billing period it is no longer in force for, absent while it stands.
    fn exclusive_end_billing_period(&self) -> Option<&str>;

    /// Does this record still stand — has nothing ended it?
    fn is_live(&self) -> bool {
        self.exclusive_end_billing_period().is_none()
    }

    /// Is this record in force in `billing_period`? Half-open, as
    /// [`billing_period_range_covers`] reads it: every period from the one it started
    /// in up to, but not including, the one it ended at — so a record that began and
    /// ended in the SAME period is in force in no period at all.
    fn covers(&self, billing_period: &str) -> bool {
        billing_period_range_covers(
            Some(self.inclusive_start_billing_period()),
            self.exclusive_end_billing_period(),
            billing_period,
        )
    }

    /// Is this record in force in ANY billing period at all? One that still stands
    /// always is; an ended one only when it ended in a LATER period than the one it
    /// started in, since the range is half-open.
    fn covers_any_period(&self) -> bool {
        billing_period_precedes(
            billing_period_key(self.inclusive_start_billing_period()),
            self.exclusive_end_billing_period()
                .and_then(billing_period_key),
        )
    }
}

/// Prune `records`, the history one resource keeps of the stretches it was in force
/// over (oldest first): the records in force in no billing period at all go, and then
/// at most `max_ended` ENDED ones are kept, the oldest ended record dropped first.
///
/// The ONE place either history is bounded, so the record still in force is never the
/// one evicted however many times a caller cycles a resource inside one billing period:
/// a record covering nothing is not kept at all, and a record covering something is
/// only ever dropped once `max_ended` NEWER ended records exist to report the later
/// periods from.
pub fn prune_period_history<Record: BillingPeriodHistoryRecord>(
    records: &mut Vec<Record>,
    max_ended: usize,
) {
    records.retain(Record::covers_any_period);
    while ended_count(records) > max_ended {
        let Some(oldest) = records.iter().position(|record| !record.is_live()) else {
            break;
        };
        records.remove(oldest);
    }
}

/// How many of `records` have ENDED — the history `max_ended` bounds in
/// [`prune_period_history`].
fn ended_count<Record: BillingPeriodHistoryRecord>(records: &[Record]) -> usize {
    records.iter().filter(|record| !record.is_live()).count()
}
