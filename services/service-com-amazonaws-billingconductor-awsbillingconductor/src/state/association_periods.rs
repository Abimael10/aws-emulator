// SPDX-License-Identifier: CC0-1.0

//! Putting SEVERAL stretches of billing periods into one resource association at once —
//! what a call that bounds its range to the periods two resources share has to write,
//! since bounding one range can leave several stretches.

use super::*;

/// Put every stretch of `added` INTO `spans`, the stretches of billing periods ONE
/// resource is in effect over, and report what that did to them as a whole.
///
/// Each stretch goes in through the addition every store here shares
/// ([`add_billing_periods`]), so the spans stay disjoint and ascending and a stretch
/// every period of which is already in effect writes nothing. The whole addition is
/// REFUSED at the [`MAX_BILLING_PERIOD_SPANS`] bound, and nothing of it is written until
/// every stretch is through: one call putting an association in effect over several
/// stretches leaves it in effect over all of them or over exactly the ones it already
/// held, never over a prefix of what it asked for.
pub fn add_billing_period_spans(
    spans: &mut Vec<BillingPeriodSpan>,
    added: &[BillingPeriodSpan],
) -> PeriodsAddition {
    let mut widened = spans.clone();
    let mut outcome = PeriodsAddition::Unchanged;
    for span in added {
        match add_billing_periods(&mut widened, span) {
            PeriodsAddition::Changed => outcome = PeriodsAddition::Changed,
            PeriodsAddition::Unchanged => {}
            PeriodsAddition::AtSpanLimit => return PeriodsAddition::AtSpanLimit,
        }
    }
    if outcome == PeriodsAddition::Changed {
        *spans = widened;
    }
    outcome
}

impl ResourceAssociationRecord {
    /// Put every stretch of `added` INTO this association, and report what that did to
    /// the periods it is in effect in ([`add_billing_period_spans`]).
    ///
    /// [`Self::add_periods`] for the caller that has already worked out the STRETCHES the
    /// association is to be in effect over rather than one range to be bounded below —
    /// the periods a range and the two resources the association joins all share
    /// ([`ComAmazonawsBillingconductorAwsbillingconductorState::associable_billing_periods`]),
    /// which can be several stretches when either resource applies in several — exactly
    /// as [`Self::over`] is [`Self::new`] for that caller.
    pub fn add_period_spans(&mut self, added: &[BillingPeriodSpan]) -> PeriodsAddition {
        add_billing_period_spans(&mut self.spans, added)
    }
}
