Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 41fa950ddd6915a1c40615232d76af513e3c005f

# The nextToken is scoped only to the read that minted it, so a token replayed with another billingMonth silently drops that month's linked accounts

_Filed: 2026-11-15T23:32:41Z_
_Operation: ListEnterpriseSupportLinkedAccountCharges | Class: pagination_

> _Note: this issue was identified against the code base at commit `41fa950ddd6915a1c40615232d76af513e3c005f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListEnterpriseSupportLinkedAccountCharges` walks the same `support_eligible_spend` store, keyed `"<billingMonth>\u{1f}<accountId>"`, and seeks to the required `billingMonth`'s key range. The `nextToken` it mints carries only the read's own name and that raw key, so the month that SELECTED the sequence is not part of the cursor's identity:

* a token minted for `billingMonth=2024-03` and replayed with an earlier month makes `*key > after` false for every key of that month, so the read answers HTTP 200 with `linkedAccount: []` and no `nextToken` — the month's entire breakdown is dropped instead of the request being refused;
* replayed with a later month, every key passes and the cursor is silently ignored, restarting the walk at the first account of that month.

The crate's own test suite notes that these two Support charge reads "page over the same spend store, so their payloads are drawn from one key space and only the read named in the token tells their cut points apart" — within ONE read, nothing tells one month's cut points from another's. Two sibling reads of this crate do bind the thing that selects their sequence into the token (`ListSourceViewsForBillingView` the view's ARN, `GetBillingPreferences` a digest of its filters) and refuse a token that names another one.

## Evidence

The cursor is the bare store key, decoded before the month is known to matter — services/service-com-amazonaws-billing-awsbilling/src/ops/list_enterprise_support_linked_account_charges.rs:373-377:
```
        let page_key = state.page_token_key();
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(resume_key(&page_key, Self::NAME, token)?),
            None => None,
        };
```
...the month-scoped walk — list_enterprise_support_linked_account_charges.rs:388-389:
```
        let selected = state
            .enterprise_support_spend_entries_for_month(&payer_account_id, &input.billing_month)
```
...and the resume filter and mint over that key — list_enterprise_support_linked_account_charges.rs:401-406:
```
        let mut remaining =
            selected.filter(|(key, _)| resume_after.as_ref().is_none_or(|after| *key > after));
        let page: Vec<(&String, &SupportEligibleSpend)> = remaining.by_ref().take(limit).collect();
        let next_token = if remaining.next().is_some() {
            page.last()
                .map(|(key, _)| crate::state::encode_page_token(&page_key, Self::NAME, key))
```
The key whose first segment is the month — services/service-com-amazonaws-billing-awsbilling/src/state/support_charges.rs:160-162:
```
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}", self.billing_month, self.account_id)
    }
```
The codec that binds only the read name — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1708-1712:
```
pub fn decode_page_token(key: &PageTokenKey, read_by: &str, token: &str) -> Option<String> {
    let cursor = key.cursor_of(&::hex::decode(token).ok()?)?;
    let (minted_by, payload) = cursor.split_once(PAGE_TOKEN_SCOPE_SEPARATOR)?;
    (minted_by == read_by).then(|| payload.to_string())
}
```
The sibling that does scope its token to the sequence — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:271-276:
```
    if stored_arn != arn {
        return Err(invalid_field(
            "nextToken",
            "the pagination token was issued for another billing view",
        ));
    }
```
Test coverage: tests/all/token_scope.rs:20-23 "The two Support charge reads are the sharpest rows: they page over the same spend store, so their payloads are drawn from one key space and only the read named in the token tells their cut points apart" — the table crosses each read with OTHER reads' tokens only (tests/all/token_scope.rs:6-12), never with its own token under a different `billingMonth`.

## Steps to Reproduce

1. Boot the emulator with the caller as the billing family's payer account (the harness in tests/all/token_scope.rs).
2. Seed Enterprise Support spend (`support_plan_name = AWSSupportEnterprise`) for several family accounts in two past months, e.g. 2024-02 and 2024-03.
3. Call ListEnterpriseSupportLinkedAccountCharges(billingMonth="2024-03", maxResults=1) and keep the `nextToken`.
4. Call ListEnterpriseSupportLinkedAccountCharges(billingMonth="2024-02", nextToken=<that token>).
5. Observe HTTP 200 with `linkedAccount: []` and no `nextToken`, although February holds priced accounts — instead of the modeled ValidationException for a token that cuts another sequence.
