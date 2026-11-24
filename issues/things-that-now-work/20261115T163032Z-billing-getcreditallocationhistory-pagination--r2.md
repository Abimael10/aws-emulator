Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: bf13e2b1e3644c24b1eab3f3e1e978e2f3ab3b1f

# The nextToken is scoped only to the read that minted it, so a token reused with another accountId/creditId/date range is honoured as a cut point and silently drops selected entries

_Filed: 2026-11-15T16:30:32Z_
_Operation: GetCreditAllocationHistory | Class: pagination_

> _Note: this issue was identified against the code base at commit `bf13e2b1e3644c24b1eab3f3e1e978e2f3ab3b1f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetCreditAllocationHistory` pages by carrying the store key of the previous page's last entry in the token. The only scoping the token gets is the NAME of the read that minted it: `state::decode_page_token` accepts the cursor when `minted_by == read_by` and returns the payload unchecked, and `resume_key` adds nothing. The members that actually SELECT the sequence this read walks — the required `accountId`, the optional `creditId` filter and the required `startDate`/`endDate` window — never travel in the token and are never compared against the resuming request. The carried key is then applied as the EXCLUSIVE upper bound of whatever range the new request asked for (`let upper = match resume_after { Some(after) if after < end_upper => after, _ => end_upper }`), so every entry the new selection contains above that key is silently dropped and the answer is a 200 whose list omits them.

The sharpest case is a widened or shifted date window: a token minted while walking 2025-01..2025-02 and presented with endDate in 2025-03 clamps the walk at the February cut point, so every March entry the new range selects is never reported and the page carries no nextToken to reach them. Changing `accountId` (a payer that switches from one family member's history to the family-wide one) or adding a `creditId` filter drops entries the same way, because the cut is a key comparison and not a position in the newly selected sequence.

Two sibling paginated reads of this same crate refuse exactly this misuse, and one states the rule in general terms: `GetBillingPreferences` carries the feature plus a digest of the filters and refuses "the pagination token was issued for another feature" / "another set of filters" because "a cursor is a position among the entries the request selects, so a token is usable only by a request that selects the same ones: the scope covers every member that narrows the sequence"; `ListSourceViewsForBillingView` carries the walked view's ARN and refuses "the pagination token was issued for another billing view". The comment above the clamp here even reasons about a token "below the requested range" and one "at or above the range" — i.e. about a caller that changed the range between pages — and stops short of the case that loses data. This is a different defect from the already-fixed GetCreditAllocationHistory pagination issue (which made the token authenticated so a token the service never issued is refused) and it is not one of the three siblings enumerated in the open ListBusinessSupportSubscriptionHistory pagination issue.

## Evidence

The token is taken back on the operation name alone — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1708-1712:
```
pub fn decode_page_token(key: &PageTokenKey, read_by: &str, token: &str) -> Option<String> {
    let cursor = key.cursor_of(&::hex::decode(token).ok()?)?;
    let (minted_by, payload) = cursor.split_once(PAGE_TOKEN_SCOPE_SEPARATOR)?;
    (minted_by == read_by).then(|| payload.to_string())
}
```
and this read adds no further check — services/service-com-amazonaws-billing-awsbilling/src/ops/get_credit_allocation_history.rs:203-219 (`resume_key` just forwards to it) and :365-368:
```
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(resume_key(&page_key, Self::NAME, token)?),
            None => None,
        };
```
The carried key becomes the bound of whatever range the NEW request selected — get_credit_allocation_history.rs:380-402:
```
        let (lower, _) = crate::state::CreditAllocation::month_key_bounds(&start_month);
        let (_, end_upper) = crate::state::CreditAllocation::month_key_bounds(&end_month);
        // ... A token
        // naming a key below the requested range leaves an empty range, which
        // the guard below answers as an empty page; one at or above the range
        // leaves the whole range still to come, which is what the descending
        // walk owes a caller that narrowed the range after that page.
        let upper = match resume_after {
            Some(after) if after < end_upper => after,
            _ => end_upper,
        };
        let mut page: Vec<(&String, &crate::state::CreditAllocation)> = Vec::new();
        if lower < upper {
            for (key, allocation) in state.credit_allocations.range(lower..upper).rev() {
```
and the other two selecting members are applied per entry, not carried in the token — get_credit_allocation_history.rs:274-283:
```
fn is_selected(
    state: &crate::state::ComAmazonawsBillingAwsbillingState,
    allocation: &crate::state::CreditAllocation,
    input: &GetCreditAllocationHistoryInput,
) -> bool {
    state.history_covers_billed_account(&input.account_id, &allocation.account_id)
        && input
            .credit_id
            .is_none_or(|credit| is_filtered_credit(&allocation.credit_id, credit))
}
```
The sibling rule this breaks — services/service-com-amazonaws-billing-awsbilling/src/ops/get_billing_preferences.rs:326-332:
```
/// A cursor is a position among the entries the request selects, so a token is
/// usable only by a request that selects the same ones: the scope covers every
/// member that narrows the sequence — here the filtered keys, digested in one
/// canonical order ... `maxResults` is deliberately NOT part of it
```
enforced at get_billing_preferences.rs:248-259:
```
        if stored_feature != feature {
            return Err(invalid_field(
                "nextToken",
                "the pagination token was issued for another feature",
            ));
        }
        if stored_scope != scope {
            return Err(invalid_field(
                "nextToken",
                "the pagination token was issued for another set of filters",
            ));
        }
```
and the same rule for the member that selects a sequence — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:271-276:
```
    if stored_arn != arn {
        return Err(invalid_field(
            "nextToken",
            "the pagination token was issued for another billing view",
        ));
    }
```
Test coverage today: tests/all/token_scope.rs asserts only cross-READ and never-issued tokens (its header states the rule as "a paginated read resumes a walk only from a nextToken THAT READ issued, and refuses every other token — one this service cannot read back, and one another of its reads minted"), and tests/all/credit_allocation_history.rs pages with the same request body throughout. No test presents this read a token of its own minted under another accountId, creditId or date range, so nothing in the crate justifies the divergence from the two reads above.

## Steps to Reproduce

1. Boot the emulator with the harness in tests/all/credit_allocation_history.rs (one router over one shared engine).
2. Seed credit allocations for the caller's own bills in three billing months with `record_credit_allocation`: 2025-01, 2025-02 and 2025-03.
3. POST GetCreditAllocationHistory with {"accountId":"000000000000","startDate":<2025-01-01>,"endDate":<2025-02-15>,"maxResults":1}. The answer carries the 2025-02 entry and a nextToken whose payload is that entry's store key ("2025-02\u{1f}...").
4. POST GetCreditAllocationHistory again with the SAME accountId but a widened window — {"startDate":<2025-01-01>,"endDate":<2025-03-15>,"nextToken":<that token>}.
5. Observe 200 with a creditAllocationHistoryList that starts at 2025-01: every 2025-03 entry the widened range selects is silently dropped (the carried February key became the walk's exclusive upper bound) and no nextToken is offered to reach them — while the same request WITHOUT the token reports 2025-03 first.
6. Repeat with the token minted for one accountId and presented with another (a payer switching between a member's history and the family-wide one), and with a creditId filter added between pages, for the same silent omission.
7. Compare with GetBillingPreferences (get_billing_preferences.rs:248-259) and ListSourceViewsForBillingView (list_source_views_for_billing_view.rs:271-276), which answer the modeled ValidationException for a token minted under another selection instead of serving a truncated page.
