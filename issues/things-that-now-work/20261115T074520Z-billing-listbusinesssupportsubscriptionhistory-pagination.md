Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 5d7ae394f187f060d35765f1ed4ed8891185905a

# The nextToken is scoped only to the read that minted it, so a token reused with another billingMonth/date window is honoured as a cut point and silently drops matching contracts

_Filed: 2026-11-15T07:45:20Z_
_Operation: ListBusinessSupportSubscriptionHistory | Class: pagination_

> _Note: this issue was identified against the code base at commit `5d7ae394f187f060d35765f1ed4ed8891185905a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

ListBusinessSupportSubscriptionHistory pages over the contracts read off the stored bills, keyed by `SupportSubscriptionContract::key()` = account + first billing month. The only scoping its `nextToken` carries is the NAME of the read that minted it: `crate::state::decode_page_token` splits the authenticated cursor on the scope separator and accepts it when `minted_by == read_by`, returning the payload unchecked. The members that actually SELECT the sequence this read walks — `billingMonth` (or the `startDate`/`endDate` window that replaces its bounds) and the `accountId` filter — are not carried in the token and are never compared with the resuming request. So a token minted while walking one window is applied as a cut point in a DIFFERENT window's list: every contract whose key sorts at or below the carried key is filtered out of the page even though the new window selects it, and the service answers 200 with a required `subscriptionContracts` list that silently omits them (and a `nextToken` computed from that truncated walk) instead of refusing the token.

Two sibling paginated reads in this same crate refuse exactly this misuse, and one of them states the rule in general terms: `ListSourceViewsForBillingView` carries the walked view's ARN in the token and refuses "the pagination token was issued for another billing view"; `GetBillingPreferences` carries the feature plus a digest of the filters and refuses "the pagination token was issued for another feature" / "another set of filters", because "a cursor is a position among the entries the request selects, so a token is usable only by a request that selects the same ones: the scope covers every member that narrows the sequence".

The same shape and the same gap are in this read's three siblings, which also bind their token to nothing but the operation name: `ListBillingViewSegments` (whose `arn` and `timeRange` select the sequence — a token minted over a wide `timeRange` resumed with a narrower one skips segments the new range selects), `ListBusinessSupportAccountCharges` and `ListEnterpriseSupportLinkedAccountCharges` (whose required `billingMonth` selects the sequence — a cross-month token either restarts the walk or empties the page while `accountCount`/`totalSupportCharge` still report the month's entries). This read is the sharpest case because its cursor does not even embed the month, so the omission happens mid-sequence rather than as a restart.

## Evidence

The token this read mints and takes back is scoped to the operation NAME only — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1708-1712:
```
pub fn decode_page_token(key: &PageTokenKey, read_by: &str, token: &str) -> Option<String> {
    let cursor = key.cursor_of(&::hex::decode(token).ok()?)?;
    let (minted_by, payload) = cursor.split_once(PAGE_TOKEN_SCOPE_SEPARATOR)?;
    (minted_by == read_by).then(|| payload.to_string())
}
```
and this read adds no further check — services/service-com-amazonaws-billing-awsbilling/src/ops/list_business_support_subscription_history.rs:184-195 (the doc claims only cross-READ scoping: "one it minted for another of its paginated reads, whose payload is a cut point of another sequence"):
```
/// The contract key a `nextToken` resumes after, or a `ValidationException`
/// when the token is not one THIS read issued under this store's key — one this
/// service cannot read back, one a caller assembled, and one it minted for
/// another of its paginated reads, whose payload is a cut point of another
/// sequence. ...
fn resume_key(...) { crate::state::decode_page_token(key, read_by, token).ok_or_else(|| ... ) }
```
The carried key is then applied to whatever window the new request selected — list_business_support_subscription_history.rs:297-306 and :358-360:
```
        let window_start = start_filter.unwrap_or_else(|| crate::state::date_time_secs(&month_start));
        let window_end = end_filter.unwrap_or_else(|| crate::state::date_time_secs(&month_end));
...
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(resume_key(&page_key, Self::NAME, token)?),
            None => None,
        };
...
        let mut remaining = selected
            .into_iter()
            .filter(|(key, _)| resume_after.as_ref().is_none_or(|after| key > after));
```
and the key it compares carries no window at all — services/service-com-amazonaws-billing-awsbilling/src/state/support_subscriptions.rs:52-54:
```
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}", self.account_id, self.first_billing_month)
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
The same unscoped resume filter in the siblings: list_business_support_account_charges.rs:525-527, list_enterprise_support_linked_account_charges.rs:401-402, list_billing_view_segments.rs:399-405.

Test coverage today: tests/all/business_support_subscription_history.rs:640 `pages_the_contracts_by_max_results` walks the pages with the SAME request body (it only sets `request["nextToken"]` at :675), and :715 `refuses_a_request_it_cannot_answer` covers only "a nextToken this service never issued" (:740-744). No test pins a token reused with another `billingMonth` or date window, so nothing in the crate justifies the divergence from the two reads above.

## Steps to Reproduce

1. Seed the store as tests/all/business_support_subscription_history.rs does: add two further accounts to `consolidated_billing_family` and `record_support_eligible_spend` an `AWSSupportBusiness` bill for accounts 000000000000, 111111111111 and 222222222222 in both 2024-01 and 2024-02 (so each account has one contract spanning both months).
2. POST ListBusinessSupportSubscriptionHistory with {"billingMonth":"2024-01","maxResults":1} and keep the returned nextToken T (its payload is "000000000000\u{1f}2024-01").
3. POST ListBusinessSupportSubscriptionHistory with {"billingMonth":"2024-02","nextToken":T}: the call answers 200 and reports only the 111111111111 and 222222222222 contracts — account 000000000000's contract, which the same request WITHOUT the token reports for 2024-02, is silently dropped because its key sorts at the cut point of the January walk.
4. Compare with the two reads that refuse a token minted under another selection: ListSourceViewsForBillingView (list_source_views_for_billing_view.rs:271-276) and GetBillingPreferences (get_billing_preferences.rs:248-259), which answer the modeled ValidationException instead of a page.
5. Repeat with {"billingMonth":"2024-01","startDate":...,"endDate":...} vs a different date window, and with ListBillingViewSegments using a token minted over a wide timeRange and resumed with a narrower one, for the same silent omission in the sibling reads.
