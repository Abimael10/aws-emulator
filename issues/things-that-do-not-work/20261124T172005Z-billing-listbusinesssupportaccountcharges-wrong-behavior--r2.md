Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: f8a8db808fb2efa4ac1d60fcd59e02b605ad9cca

# isEstimated is derived only from the calendar month and never reads the stored is_estimated_bill flag of the bills it prices, unlike the sibling charge summary

_Filed: 2026-11-24T17:20:05Z_
_Operation: ListBusinessSupportAccountCharges | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `f8a8db808fb2efa4ac1d60fcd59e02b605ad9cca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListBusinessSupportAccountCharges` prices every stored `SupportEligibleSpend` record of the requested month, but computes the response's `isEstimated` from the month's position on the calendar alone (`requested == RequestedMonth::Current`). It never looks at `SupportEligibleSpend::is_estimated_bill` — the stored per-bill flag this crate defines as "`true` when the account's bill for the month is not yet finalized, so the charges computed from this record are estimates" — although the same loop borrows each record and reads every other field of it.

Consequence: a PAST billing month whose stored bills are flagged as not-yet-finalized is answered with `isEstimated: false`, i.e. the response asserts "the charge amount is finalized" about charges this service's own store says are estimates. The sibling read of the SAME store, for the SAME documented member and documented sentence, does exactly the opposite: `GetEnterpriseSupportChargeSummary` ORs the stored flag over the bills it prices, and a crate test pins that rule (`estimates_a_month_while_any_bill_it_prices_is_not_final`). `GetCreditAllocationHistory` likewise reports the stored flag per entry (`is_estimated_bill: allocation.is_estimated_bill`). So one reader of the flag honours it and this one drops it, and the two reads of one month disagree.

Full disclosure for a fixer: the handler's own comment justifies the month-based rule from the `BusinessSupportBillingMonth` shape doc ("Supports the current month (estimated charges) and past months (finalized charges)"), and `tests/all/business_support_account_charges.rs` pins current-month → true and a past month → false. Neither test seeds a past month bill carrying `is_estimated_bill: true`, so the contradicting case is unpinned: keeping the current-month rule AND ORing the stored flag onto it leaves both existing tests and both modelled sentences true.

## Evidence

Model, the member's documented meaning (smithy_query query="doc" shape="ListBusinessSupportAccountChargesResponse" member="isEstimated"):
```
"Specifies whether the Support charge amount is estimated. When false, the charge amount is finalized."
```
(the identical sentence documents `GetEnterpriseSupportChargeSummaryResponse$isEstimated`).

The stored flag this crate persists per account-month bill — services/service-com-amazonaws-billing-awsbilling/src/state/support_charges.rs:134-137:
```
    /// `true` when the account's bill for the month is not yet finalized, so
    /// the charges computed from this record are estimates — the same reading
    /// of a bill [`CreditAllocation::is_estimated_bill`] carries.
    pub is_estimated_bill: bool,
```

The sibling read that honours it — services/service-com-amazonaws-billing-awsbilling/src/ops/get_enterprise_support_charge_summary.rs:311-313:
```
            // "When false, the charge amount is finalized": the month's charge
            // is an estimate while any bill it is computed from still is.
            is_estimated |= spend.is_estimated_bill;
```

This handler, which ignores it — services/service-com-amazonaws-billing-awsbilling/src/ops/list_business_support_account_charges.rs:516:
```
        let is_estimated = requested == RequestedMonth::Current;
```
and its loop over the same records (:550-566) reads `account_charge(spend)`, the charge, the basis and `spend.account_id`, but never `spend.is_estimated_bill`:
```
        for (key, spend) in state.business_support_spend_entries_for_month(
            crate::state::caller_account_id(),
            &input.billing_month,
        ) {
            let (charge, charge_minor, basis_minor) = account_charge(spend);
```

Test coverage. The sibling's rule is pinned — services/service-com-amazonaws-billing-awsbilling/tests/all/enterprise_support_charge_summary.rs:553-559:
```
async fn estimates_a_month_while_any_bill_it_prices_is_not_final() {
    let cases: [(&str, &str, bool, bool); 2] = [
        ("every bill final", PAST_MONTH, false, false),
        ("one bill still an estimate", EARLIER_MONTH, true, true),
    ];
```
This operation's test pins only the two cases that do not conflict — services/service-com-amazonaws-billing-awsbilling/tests/all/business_support_account_charges.rs:924 (`is_estimated_bill: false` for the current month) and :963-967:
```
    assert_eq!(
        past["isEstimated"].as_bool(),
        Some(false),
        "case \"past month\": expected a past month to be finalized, got {past}"
    );
```
where that past month has NO seeded bill at all, so no stored flag is in play.

The justification the handler states (disclosed, and not contradicted by the fix sketched above) — list_business_support_account_charges.rs:492-498: "the shape documents the current month (estimated charges) and past months (finalized charges) ... the current month is exactly the one whose charges are still estimated."

## Steps to Reproduce

1. Boot the emulator with the harness in tests/all/business_support_account_charges.rs (`emulator()`).
2. Seed one PAST billing month's Business Support bill with the estimate flag set: `state.record_support_eligible_spend(SupportEligibleSpend { account_id: caller_account_id(), billing_month: month_from_now(-2), support_plan_name: SUPPORT_PLAN_BUSINESS, currency_code: "USD", by_service: vec![/* any spend */], discount: None, reserved_instance_spend_minor: 0, savings_plan_spend_minor: 0, is_estimated_bill: true })`.
3. Call ListBusinessSupportAccountCharges with `{ "billingMonth": month_from_now(-2) }`.
4. Observe `isEstimated: false` in the response, i.e. "the charge amount is finalized", although the only bill the month's charges were computed from carries `is_estimated_bill: true`.
5. Re-seed the same account-month with `support_plan_name: SUPPORT_PLAN_ENTERPRISE` and call GetEnterpriseSupportChargeSummary for that month: the sibling read of the same store answers `isEstimated: true` for the same flag.
