Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: 9e4314a6b919d5458fdf4a3f91d7ac4b6bc489f9

# Nothing in src/ ever records a credit allocation, so the credit-allocation history is empty for every request a client can make — including after RedeemCredits mints a credit

_Filed: 2026-11-23T14:42:50Z_
_Operation: GetCreditAllocationHistory | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `9e4314a6b919d5458fdf4a3f91d7ac4b6bc489f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetCreditAllocationHistory` reports from `ComAmazonawsBillingAwsbillingState::credit_allocations`, and no production path in this crate ever writes an entry there: the only caller of the store's writer `record_credit_allocation` is this crate's test suite, in-process. Every wire request therefore answers 200 with `creditAllocationHistoryList: []`, `partialResults: false`, `failedMonths: []`, whatever account, credit id or date range it names — the family traversal, the month-keyed reverse walk, the 24-month gate and the pagination are all exercised over an always-empty store.

This is not merely "a fresh account has no history": the one credit-producing production path the crate HAS cannot feed it. `RedeemCredits` mints a real credit into the ledger (`store.insert_credit(...)`, redeem_credits.rs:203) and its own handler doc quotes the model's promise that "The credit is then automatically applied to subsequent bills according to the standard credit application order" — but nothing ever records that application, so a client that redeems a code and then reads the allocation history is told the credit was applied to nothing, ever.

## Evidence

The store's field doc admits the gap — services/service-com-amazonaws-billing-awsbilling/src/state.rs:701-707:

    /// Every persisted credit allocation, keyed and ordered by
    /// [`CreditAllocation::key`] so `GetCreditAllocationHistory`'s page
    /// boundaries are stable. Bounded by [`CreditAllocation::MAX_RESIDENT`].
    ///
    /// Empty on a fresh emulator: an allocation is written when a credit is
    /// applied to a bill, and this emulator runs no billing pipeline, so only a
    /// future operation that applies credits adds entries here.

The writer exists at src/state.rs:976 (`pub fn record_credit_allocation(&mut self, allocation: CreditAllocation) -> bool`), and a repo-wide grep for `record_credit_allocation` finds ONLY that declaration plus test call sites: tests/all/credit_allocation_history.rs:253, tests/all/credit_allocation_family.rs:148, tests/all/token_scope.rs:209. No `src/` caller anywhere in the workspace.

The read is wholly keyed on that store — src/ops/get_credit_allocation_history.rs:392-402:

        let mut page: Vec<(&String, &crate::state::CreditAllocation)> = Vec::new();
        if lower < upper {
            for (key, allocation) in state.credit_allocations.range(lower..upper).rev() {

and src/ops/get_credit_allocation_history.rs:411-433 reports that page with `partial_results: false` and `failed_months: Some(Vec::new())`.

The credit side DOES have a production writer, which makes the missing half concrete — src/ops/redeem_credits.rs:196-203:

        // "On success, a new credit is added to the account's credit ledger with the
        // amount, validity period, and applicable products defined by the promotion."
        ...
        let stored = store.insert_credit(crate::state::Credit {

while the same file's header carries the model sentence that is never honoured — src/ops/redeem_credits.rs:52-54:

    //! Redeems an Amazon Web Services promotional credit code ... The credit is then
    //! automatically applied to subsequent bills according to the standard credit application
    //! order.

Test coverage: tests/all/credit_allocation_history.rs:674-694 (`an_account_with_no_persisted_allocations_gets_an_empty_list`) pins the empty answer, and every non-empty assertion in that suite and in credit_allocation_family.rs first seeds the store in-process through `record_credit_allocation` — a path no wire client has.

## Steps to Reproduce

1. Boot the emulator with only wire access and sign as the emulator account.
2. Issue a promotional credit code through the partner-benefits service and redeem it with `RedeemCredits` — `GetCredits` then reports the minted credit, so the ledger write is observable.
3. Call `GetCreditAllocationHistory` with `{"accountId": "<caller>", "startDate": <a past instant>, "endDate": <now>}` — 200 with `creditAllocationHistoryList: []`, `partialResults: false`, `failedMonths: []`.
4. Repeat with any `creditId` filter, any account of the billing family, and any range up to 24 billing months wide: the answer is always the empty list.
5. grep `record_credit_allocation` under `services/*/src` — the only hit is the declaration at src/state.rs:976, so no sequence of client calls can put an entry in the store the read walks.
