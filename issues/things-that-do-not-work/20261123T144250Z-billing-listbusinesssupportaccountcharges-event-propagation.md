Issue-Severity: HIGH
Issue-Gap-Class: event-propagation
Issue-Commit: 9e4314a6b919d5458fdf4a3f91d7ac4b6bc489f9

# Nothing in src/ ever records Support-eligible spend, so all four Support charge reads answer an empty/zeroed month for every request a client can make

_Filed: 2026-11-23T14:42:50Z_
_Operation: ListBusinessSupportAccountCharges | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `9e4314a6b919d5458fdf4a3f91d7ac4b6bc489f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every Support charge report of this service computes its answer from `ComAmazonawsBillingAwsbillingState::support_eligible_spend`, and no production path in this crate (or anywhere in the workspace) ever writes a record into it. The ONLY caller of the store's writer `record_support_eligible_spend` is this crate's own test suite, which reaches into the engine in-process; nothing a wire client can call records a bill.

The consequence is four of this service's 23 operations are vacuous over the wire, each answering 200 with nothing in it whatever the caller asks:

* `ListBusinessSupportAccountCharges` — `accountCharges: []`, `accountCount: 0`, `totalSupportCharge: "0.00"`, `totalSupportEligibleSpend: "0.00"` for every billing month (the whole tier-pricing schedule in the handler is unreachable).
* `ListEnterpriseSupportLinkedAccountCharges` — `linkedAccount: []` for every month, so no `billableSeconds`/proration is ever reported.
* `ListBusinessSupportSubscriptionHistory` — contracts are read OFF those same bills, so `subscriptionContracts` is always `[]`.
* `GetEnterpriseSupportChargeSummary` — with no spend record the handler's `subscribed_account_months == 0` branch answers `EnterpriseSupportCharge::default()`, i.e. `supportCharge`/`totalSupportCharge`/`totalSupportEligibleSpend`/every per-category spend member `"0.00"` and `supportChargePercentage` `"0.00"`, for every past month.

The store, the writer, the per-month key range and both pricing engines all exist; what is missing is the production path that records an account-month's Support-eligible spend (the equivalent of the cross-service wiring `RedeemCredits` already has, which spends a code the partner-benefits service ISSUED through a modelled operation of that service).

## Evidence

The store's own field doc admits the gap — services/service-com-amazonaws-billing-awsbilling/src/state.rs:749-757:

    /// Every persisted account-month of Support-eligible spend, keyed and
    /// ordered by [`SupportEligibleSpend::key`] (billing month, then account)
    /// so `ListBusinessSupportAccountCharges` seeks straight to one month and
    /// its page boundaries are stable. ...
    ///
    /// Empty on a fresh emulator: spend accrues from a billing run, which this
    /// emulator does not perform. The Support CHARGE is not held here — it is
    /// computed from this spend by the subscribed plan's price schedule.

and so does the module — src/state/support_charges.rs:15-19:

    //! Empty on a fresh emulator: spend accrues from a billing pipeline, which this
    //! emulator does not run, so only a future operation that bills usage — or this
    //! service's test suite, through
    //! [`ComAmazonawsBillingAwsbillingState::record_support_eligible_spend`] —
    //! writes it. Nothing synthesises a record.

and the writer itself — src/state/support_charges.rs:206-212:

    /// Answers `false` without writing when the store is already at
    /// [`SupportEligibleSpend::MAX_RESIDENT`], so the caller refuses rather
    /// than growing the store without a bound. The only writer today is this
    /// service's test suite: spend accrues from a billing run, which this
    /// emulator does not perform, so a future operation that bills usage is
    /// what will call this in production.
    pub fn record_support_eligible_spend(&mut self, spend: SupportEligibleSpend) -> bool {

A repo-wide grep for `record_support_eligible_spend` finds ONLY that declaration, the doc references above, and call sites under `tests/all/` (business_support_account_charges.rs:207/910/1020, enterprise_support_charge_summary.rs:211, enterprise_support_linked_account_charges.rs:245/1151, business_support_subscription_history.rs:135/489, token_scope.rs:203/759). No `src/` caller anywhere in the workspace.

The four readers are all keyed on that store:
* src/ops/list_business_support_account_charges.rs:541-543 — `for (key, spend) in state.support_eligible_spend_for_month(crate::state::caller_account_id(), &input.billing_month)` — and 576-584 builds `accountCount`/`totalSupportCharge`/`totalSupportEligibleSpend` purely from that loop.
* src/ops/list_enterprise_support_linked_account_charges.rs:427-428 — `state.enterprise_support_spend_entries_for_month(&payer_account_id, &input.billing_month)`.
* src/state/support_subscriptions.rs:16-21 — "Empty on a fresh emulator, for the reason the `support_charges` module gives: ... Nothing here synthesises a contract." (read by src/ops/list_business_support_subscription_history.rs:322-323).
* src/ops/get_enterprise_support_charge_summary.rs:353-361 — "A month in which no account of the profile was subscribed to Enterprise Support carries no Support charge" → `EnterpriseSupportCharge::default()`.

Test coverage confirms the empty answer is what a client sees: tests/all/business_support_account_charges.rs:777-797 ("A month nothing was billed in is an empty month, not a refusal") asserts the zeroed month, and every non-empty assertion in these four suites first seeds the store in-process through `record_support_eligible_spend` — a path no wire client has.

## Steps to Reproduce

1. Boot the emulator with only wire access (no in-process engine handle) and sign as the emulator account, which is its own billing family's payer account (state.rs:850).
2. Call `ListBusinessSupportAccountCharges` with `{"billingMonth": "<any past month>"}` — 200 with `accountCharges: []`, `accountCount: 0`, `totalSupportCharge: "0.00"`, `totalSupportEligibleSpend: "0.00"`.
3. Call `ListEnterpriseSupportLinkedAccountCharges` and `ListBusinessSupportSubscriptionHistory` for the same month — 200 with `linkedAccount: []` and `subscriptionContracts: []`.
4. Call `GetEnterpriseSupportChargeSummary` for the same month — 200 with every money member `"0.00"` and `supportChargePercentage` `"0.00"`.
5. Exercise every other operation the service models; none of them writes `support_eligible_spend` (grep `record_support_eligible_spend` under `services/*/src` — no hits outside the declaration in src/state/support_charges.rs:212), so no sequence of client calls can make any of the four reads report a charge.
