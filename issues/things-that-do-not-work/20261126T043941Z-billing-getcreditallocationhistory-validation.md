Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: d5ab8967aef8aaf4112acfa4915644468cf89f0a

# A startDate equal to endDate is refused 400 although both bounds are modelled INCLUSIVE and startDate's doc permits equality

_Filed: 2026-11-26T04:39:41Z_
_Operation: GetCreditAllocationHistory | Class: validation_

> _Note: this issue was identified against the code base at commit `d5ab8967aef8aaf4112acfa4915644468cf89f0a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetCreditAllocationHistory` treats its requested window as half-open with an EXCLUSIVE `endDate` and therefore refuses any request whose two bounds name the same instant, with the modelled `ValidationException` over `startDate`. The model documents the opposite for this operation: `endDate` is "Inclusive end date as Unix epoch seconds" and `startDate` is "Inclusive start date ... Must be on or before `endDate`" — a sentence that explicitly admits the equal case. So a caller asking "what was allocated at this instant" with `startDate == endDate` is refused a request the model states is well formed, instead of being answered with the allocations of the billing month that instant falls in.

The exclusive reading is also contradicted by this handler's own walk, independently of what the equal case should answer. Having declared `endDate` exclusive, the handler then seeks the key range `lower..end_upper` where `end_upper` is the UPPER bound of the month `endDate` falls in (`CreditAllocation::month_key_bounds(&end_month).1`), so the whole of that month is reported. For an `endDate` that is exactly a month's first instant (e.g. 2025-03-01T00:00:00Z) an exclusive window touches no part of March at all, yet every March allocation is reported — i.e. the handler does not implement the exclusivity it refuses the equal case on. Note also that the strict-before reading was imported from `ListBusinessSupportSubscriptionHistory`, whose own `startDate`/`endDate` docs state no inclusivity at all ("The start date to filter subscription contracts from."), so nothing carries it over to the one operation whose model does state it.

## Evidence

Model (smithy_query doc, GetCreditAllocationHistoryRequest):
- `endDate`: "Inclusive end date as Unix epoch seconds."
- `startDate`: "Inclusive start date as Unix epoch seconds. Must be on or before `endDate`. The range from `startDate` to `endDate` cannot exceed 24 billing months."

Handler, src/ops/get_credit_allocation_history.rs:384-397 — refuses the case the doc admits, and says so:
        // The requested window is HALF-OPEN: `endDate` is exclusive, so
        // `startDate` must be STRICTLY before it and a zero-width range — the
        // two bounds at one instant — selects nothing and is refused rather
        // than answered with an empty page. (The `startDate` member doc says
        // "Must be on or before `endDate`"; the recorded wire behaviour refuses
        // the equal case, and the wire is what this emulator answers as.)
        if crate::state::date_time_secs(&input.start_date)
            >= crate::state::date_time_secs(&input.end_date)
        {
            return Err(invalid_field(
                "startDate",
                "must be strictly before endDate (endDate is exclusive)",
            ));
        }
(The comment's "recorded wire behaviour" claim cites no recording, fixture or test; the two modelled sentences above are the only text either way, and they say the equal case is valid.)

The same handler does NOT implement that exclusivity — src/ops/get_credit_allocation_history.rs:446-457:
        let (lower, _) = crate::state::CreditAllocation::month_key_bounds(&start_month);
        let (_, end_upper) = crate::state::CreditAllocation::month_key_bounds(&end_month);
        ...
        let upper = match resume_after {
            Some(after) if after < end_upper => after,
            _ => end_upper,
        };
so the walk `state.credit_allocations.range(lower..upper).rev()` (line 460) covers the whole month `endDate` falls in — an INCLUSIVE end.

The sibling the reading came from has no such doc — smithy_query doc, ListBusinessSupportSubscriptionHistoryRequest$endDate: "The end date to filter subscription contracts to." — while src/ops/list_business_support_subscription_history.rs:364-367 words its own rule as "The requested window is HALF-OPEN, the reading of a date range this service's other dated history read answers with".

Test coverage that pins the current verdict (and whose stated reason is the exclusivity the model denies) — tests/all/credit_allocation_history.rs:702-712:
        (
            // The boundary of that same rule: a zero-width range selects no
            // instant at all, so it is refused rather than answered empty.
            "startDate equal to endDate",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 2, 10, 12),
                "endDate": at(2025, 2, 10, 12),
            }),
            Some("startDate"),
        ),

## Steps to Reproduce

1. Seed one credit allocation for account 000000000000 in billing month 2025-02 (as tests/all/credit_allocation_history.rs does through `ComAmazonawsBillingAwsbillingState::record_credit_allocation`).
2. POST / with X-Amz-Target: AWSBilling.GetCreditAllocationHistory and body {"accountId":"000000000000","startDate":<2025-02-10T12:00:00Z epoch>,"endDate":<the same instant>}.
3. Observe HTTP 400 ValidationException with message "InvalidArgument: startDate (DetailedCode=startDate must be strictly before endDate (endDate is exclusive))", although the modelled `startDate` doc says "Must be on or before `endDate`" and the modelled `endDate` doc says "Inclusive end date".
4. Separately, send {"accountId":"000000000000","startDate":<2025-02-01T00:00:00Z>,"endDate":<2025-03-01T00:00:00Z>} with an allocation seeded in 2025-03 as well: the March allocation is reported, so the handler does not treat `endDate` as exclusive anywhere except in the refusal above.
