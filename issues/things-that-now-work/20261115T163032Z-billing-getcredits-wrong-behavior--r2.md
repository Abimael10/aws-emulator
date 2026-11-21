Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: bf13e2b1e3644c24b1eab3f3e1e978e2f3ab3b1f

# payerAccountFlag aggregation ignores consolidated-billing-family membership, so credits owned by accounts outside the family are reported

_Filed: 2026-11-15T16:30:32Z_
_Operation: GetCredits | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `bf13e2b1e3644c24b1eab3f3e1e978e2f3ab3b1f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When `payerAccountFlag` is true and the caller pays the family's bill, `GetCredits` hands `whole_family = true` to `state::credits_for_period`, whose filter then matches EVERY credit in the store: `filter(|credit| whole_family || credit.account_id == account_id)` tests family membership nowhere. The model documents the widening as being over the consolidated billing family ("the response aggregates credits across the entire consolidated billing family"), and the helper's own doc one line above the filter states the same rule ("`whole_family` widens the selection to every account of the consolidated billing family this store holds credits for"). The code widens to every account, full stop.

This is also the one family-scoped read of the crate that does not go through `state::history_covers_billed_account`, the helper the crate calls "the family traversal every billing history in this service reads by" and which exists precisely to decide this: "An account that pays for nobody ... has only its own bills" / "a record about an account outside the consolidated billing family belongs to nobody's". `GetCreditAllocationHistory`, `ListBusinessSupportAccountCharges`, `ListEnterpriseSupportLinkedAccountCharges` and `ListBusinessSupportSubscriptionHistory` all filter through it; `GetCredits` does not.

The consequence is maximal on a default store, whose `consolidated_billing_family` holds the caller alone (state.rs:846-850): a `payerAccountFlag=true` read must then report exactly the caller's own credits, but it reports every credit persisted for any account id. This is distinct from the already-fixed GetCredits issue, which was about WHICH account the flag keys on (the caller, not `accountId`); the gate is now keyed correctly and the SET it widens to is wrong.

## Evidence

The modeled sentence — smithy_query(query="doc", shape="GetCreditsRequest", member="payerAccountFlag"):
```
When true and the caller is the management account, the response aggregates credits across the entire consolidated billing family. When false or omitted, returns only credits for the specified accountId.
```
and the operation doc — smithy_query(query="doc", shape="GetCredits"): "When the caller is the management account of a consolidated billing family and payerAccountFlag is true, the response aggregates credits across the entire family."

The handler's widening — services/service-com-amazonaws-billing-awsbilling/src/ops/get_credits.rs:223-230:
```
        let whole_family = input.payer_account_flag.unwrap_or(false)
            && state.is_billing_family_management_account(crate::state::caller_account_id());

        let credits = state
            .credits_for_period(&input.account_id, whole_family, start_secs, end_secs)
```
The store-side filter, with the family rule stated directly above it and tested nowhere in the code — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1313-1335:
```
    /// The credits `GetCredits` reports, in `creditId` order.
    ///
    /// `account_id` selects the owning account. `whole_family` widens the
    /// selection to every account of the consolidated billing family this store
    /// holds credits for ...
    pub fn credits_for_period(
        &self,
        account_id: &str,
        whole_family: bool,
        start_secs: f64,
        end_secs: f64,
    ) -> Vec<Credit> {
        self.credits
            .values()
            .filter(|credit| whole_family || credit.account_id == account_id)
            .filter(|credit| credit.overlaps_period(start_secs, end_secs))
```
The family traversal every sibling read applies and this one skips — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1113-1127:
```
    /// An account's own bills always; and, when `account_id` is the management
    /// (payer) account of the consolidated billing family, the bill of every
    /// MEMBER of that family too ... An account
    /// that pays for nobody (a member account, or an account outside the
    /// family) has only its own bills ...
    pub fn history_covers_billed_account(&self, account_id: &str, billed_account: &str) -> bool {
        billed_account == account_id
            || (self.is_billing_family_management_account(account_id)
                && self.family_account_name(billed_account).is_some())
    }
```
used by the sibling read of the same family — services/service-com-amazonaws-billing-awsbilling/src/state/support_charges.rs:227-243: "the same rule the credit-allocation history reads by: a record about a member account's bill belongs to the payer account's answer, and a record about an account outside the consolidated billing family belongs to nobody's" followed by `.filter(move |(_, spend)| self.history_covers_billed_account(account_id, &spend.account_id))`.

The default family holds the caller alone — services/service-com-amazonaws-billing-awsbilling/src/state.rs:846-850:
```
        let mut consolidated_billing_family = BTreeMap::new();
        consolidated_billing_family.insert(
            caller_account_id().to_string(),
            MANAGEMENT_ACCOUNT_NAME.to_string(),
        );
```
Test coverage: tests/all/credits.rs:584 `payer_account_flag_aggregates_the_whole_billing_family_for_the_paying_caller` pins the rows "paying caller, own account, flag true" => ["21","22","23"] (credits.rs:600-606) where 22 belongs to `LINKED_ACCOUNT_ID` and 23 to `UNKNOWN_ACCOUNT_ID`, yet its `seed` helper (credits.rs:154-164) writes only `insert_credit` and never touches `consolidated_billing_family` — so neither account is a family member. The test's stated reason is the doc sentence it quotes verbatim at credits.rs:574-582 ("aggregates credits across the entire consolidated billing family") and its own comment at credits.rs:30-32 calls LINKED_ACCOUNT_ID "A second, linked account of the same family": the rows express the documented rule, and only the missing family seeding makes them pass against the unscoped filter.

## Steps to Reproduce

1. Boot the emulator with the harness in tests/all/credits.rs::emulator() (one router over one shared engine; the caller is 000000000000, which is the family management account and the only member of `consolidated_billing_family` by default).
2. Seed one credit owned by 000000000000 and one owned by 999999999999 — an account the store holds no family record for — with `insert_credit`, exactly as tests/all/credits.rs::seed does.
3. POST GetCredits with {"accountId":"000000000000","startDate":<30 days ago>,"endDate":<now>} and observe only the caller's own credit (correct).
4. POST the same request with "payerAccountFlag": true.
5. Observe that the 999999999999 credit is reported too, although `state.consolidated_billing_family` contains only 000000000000, so the documented aggregation ("across the entire consolidated billing family") selects the caller's credit alone.
6. Compare with the sibling family-scoped read: seed a `SupportEligibleSpend` for 999999999999 and call ListBusinessSupportAccountCharges for that month — `history_covers_billed_account` leaves the non-member's bill out, which is the traversal GetCredits skips.
