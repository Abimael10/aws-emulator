Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: a22dc19d5e32998b957368c08025d606fc68b0e9

# A timeRange whose beginDateInclusive is in the future is accepted, although the member's modelled doc states it can't be, and the read then answers 200 with segments clipped to future instants

_Filed: 2026-11-15T04:30:49Z_
_Operation: ListBillingViewSegments | Class: validation_

> _Note: this issue was identified against the code base at commit `a22dc19d5e32998b957368c08025d606fc68b0e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`BillingViewSegmentTimeRange` carries two documented rules, one per member: `endDateExclusive` "must be after `beginDateInclusive`", and `beginDateInclusive` "can't be in the future". `requested_period` enforces the first (a range whose end is at or before its begin is refused with the modeled `ValidationException`) and never checks the second: nothing in the handler compares either requested bound with the clock, even though it has already read the clock to build the default period.

A request whose `timeRange` lies wholly in the future is therefore answered 200. Worse than an empty page: the seeded `PRIMARY` view's segment is unbounded on both sides, so `BillingViewSegment::overlaps` is true for every period, and the reported range is the stored segment CLIPPED to the requested bounds — so the read invents a segment whose `beginDateInclusive` and `endDateExclusive` are both in the future, for a period for which no billing data can exist, instead of refusing the request the model says is invalid.

This is also the only dated read of this crate that skips the "not in the future" rule its model states: `GetCredits` refuses a future `startDate` and a future `endDate` from the analogous member docs, and all three monthly reports refuse a `billingMonth` the clock has not reached.

## Evidence

Smithy, `BillingViewSegmentTimeRange$beginDateInclusive`: "The inclusive start of the time range. This value can't be in the future." (sibling member `endDateExclusive`: "This value must be after `beginDateInclusive`.")

Only the second sentence is enforced — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_view_segments.rs:237-255:
```
        // A bound that is not a finite instant names no period at all: ...
        if !begin.is_finite() || !end.is_finite() {
            return Err(invalid_field(
                "timeRange",
                "beginDateInclusive and endDateExclusive must each be an instant",
            ));
        }
        // The period is half-open, so a range whose end does not come after its
        // begin selects no instant at all: ...
        if begin >= end {
            return Err(invalid_field(
                "timeRange",
                "beginDateInclusive must be earlier than endDateExclusive",
            ));
        }
        Ok((begin, end))
```
(The clock is already in hand: `requested_period(&input, ::std::time::SystemTime::now())` at the same file:360-361.)

What a future window then answers — services/service-com-amazonaws-billing-awsbilling/src/state/billing_view_segments.rs:136-160:
```
    pub fn overlaps(&self, start_secs: f64, end_secs: f64) -> bool {
        self.begin_date_inclusive
            .is_none_or(|begin| epoch_secs(begin) < end_secs)
...
    pub fn clipped(&self, start_secs: f64, end_secs: f64) -> (f64, f64) {
        let begin = self.begin_date_inclusive.map(epoch_secs).unwrap_or(start_secs).max(start_secs);
        let end = self.end_date_exclusive.map(epoch_secs).unwrap_or(end_secs).min(end_secs);
```
with the seeded unbounded PRIMARY segment at services/service-com-amazonaws-billing-awsbilling/src/state.rs:837-840.

The siblings that DO enforce their modelled not-in-the-future rule — services/service-com-amazonaws-billing-awsbilling/src/ops/get_credits.rs:175-199:
```
        if start_secs > now_secs {
            return Err(field_validation_failed(START_DATE_FIELD, "startDate must be a past date"));
...
        if end_secs > now_secs {
            return Err(field_validation_failed(END_DATE_FIELD, "endDate must not be a future date"));
```
and services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_charge_summary.rs:266-272 ("must be a month in the past").

Test coverage: tests/all/billing_view_segments.rs:539-570 and :752-770 only exercise past windows and the end<=begin refusals; no row sends a future `beginDateInclusive`.

## Steps to Reproduce

1. Start the emulator (the account's PRIMARY billing view and its one unbounded BILLABLE segment are seeded by default).
2. Call `ListBillingViewSegments` with `timeRange: { beginDateInclusive: <now + 1 year>, endDateExclusive: <now + 2 years> }` and no `arn`.
3. Expected per the modelled member doc ("This value can't be in the future"): a 400 `ValidationException` naming `timeRange`.
4. Actual: HTTP 200 with one `items` entry whose `timeRange.beginDateInclusive` and `timeRange.endDateExclusive` are the requested future instants.
