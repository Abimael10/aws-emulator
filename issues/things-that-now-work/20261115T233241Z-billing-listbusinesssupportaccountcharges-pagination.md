Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 41fa950ddd6915a1c40615232d76af513e3c005f

# The nextToken is scoped only to the read that minted it, so a token replayed with another billingMonth is honoured as a cut point and the month is served empty

_Filed: 2026-11-15T23:32:41Z_
_Operation: ListBusinessSupportAccountCharges | Class: pagination_

> _Note: this issue was identified against the code base at commit `41fa950ddd6915a1c40615232d76af513e3c005f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListBusinessSupportAccountCharges` pages over `support_eligible_spend`, whose keys are `"<billingMonth>\u{1f}<accountId>"`. The required `billingMonth` input is what SELECTS the sequence being walked (the handler seeks to that month's key range), but the `nextToken` carries only the read's own name plus that raw store key — nothing about the month. The cut point is therefore compared, as a bare string, against the keys of whatever month the next request names:

* a token minted while paging `billingMonth=2024-03` and replayed with `billingMonth=2024-02` leaves `*key > after` false for EVERY key of February (all of them sort below `"2024-03…"`), so the read answers HTTP 200 with `accountCharges: []` and `nextToken: null` — while the same response still reports `accountCount`, `totalSupportCharge` and `totalSupportEligibleSpend` for the whole of February, because those are summed before the resume filter. One response states that February holds N priced accounts and then lists none of them.
* replayed with a LATER month the same token passes every key, so the cursor is silently ignored and the walk restarts from the first account of that month.

The crate already states and implements the opposite rule for the two reads whose sequence is likewise selected by a request member: `GetBillingPreferences` digests its filters into the token and refuses a token "issued for another set of filters", and `ListSourceViewsForBillingView` refuses one "issued for another billing view". This read binds nothing but the operation name.

## Evidence

The sequence is selected by `billingMonth`, and the cursor is just that month's store key — services/service-com-amazonaws-billing-awsbilling/src/ops/list_business_support_account_charges.rs:484-488:
```
        let page_key = state.page_token_key();
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(resume_key(&page_key, Self::NAME, token)?),
            None => None,
        };
```
...the walk over one month's range — list_business_support_account_charges.rs:504-506:
```
        for (key, spend) in state
            .support_eligible_spend_for_month(crate::state::caller_account_id(), &input.billing_month)
        {
```
...and the resume filter plus the mint, both over the bare key — list_business_support_account_charges.rs:525-536:
```
        let mut remaining = selected
            .into_iter()
            .filter(|(key, _)| resume_after.as_ref().is_none_or(|after| *key > after));
        let page: Vec<(&String, BusinessSupportAccountCharge)> =
            remaining.by_ref().take(limit).collect();
        let has_more = remaining.next().is_some();
        let next_token = if has_more {
            page.last()
                .map(|(key, _)| crate::state::encode_page_token(&page_key, Self::NAME, key))
```
The key the cursor is: services/service-com-amazonaws-billing-awsbilling/src/state/support_charges.rs:160-162:
```
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}", self.billing_month, self.account_id)
    }
```
The codec binds ONLY the read's name — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1692-1696 and 1708-1712:
```
pub fn encode_page_token(key: &PageTokenKey, minted_by: &str, last: &str) -> String {
    ::hex::encode(key.mint(&format!(
        "{minted_by}{PAGE_TOKEN_SCOPE_SEPARATOR}{last}"
    )))
}
...
pub fn decode_page_token(key: &PageTokenKey, read_by: &str, token: &str) -> Option<String> {
    let cursor = key.cursor_of(&::hex::decode(token).ok()?)?;
    let (minted_by, payload) = cursor.split_once(PAGE_TOKEN_SCOPE_SEPARATOR)?;
    (minted_by == read_by).then(|| payload.to_string())
}
```
The rule the crate applies to its other selection-scoped reads — services/service-com-amazonaws-billing-awsbilling/src/ops/get_billing_preferences.rs:323-332 and 254-259:
```
/// The scope of the SEQUENCE this request pages through, which travels in every
/// `nextToken` the walk hands back.
///
/// A cursor is a position among the entries the request selects, so a token is
/// usable only by a request that selects the same ones: the scope covers every
/// member that narrows the sequence
...
    if stored_scope != scope {
        return Err(invalid_field(
            "nextToken",
            "the pagination token was issued for another set of filters",
        ));
    }
```
The totals that contradict the empty page — list_business_support_account_charges.rs:495-519 computes `account_count`/`total_support_charge` over the whole month BEFORE the resume filter ("the month's `totalSupportCharge`, `totalSupportEligibleSpend` and `accountCount` are documented as being for ALL accounts in the billing month").

Test coverage: tests/all/token_scope.rs:6-12 states the only rule its table drives — "a paginated read resumes a walk only from a `nextToken` THAT READ issued, and refuses every other token — one this service cannot read back at all, and one another of its reads minted" — so no row feeds a read its OWN token with a different `billingMonth`.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/token_scope.rs).
2. Seed Support-eligible spend for several accounts in two past months, e.g. 2024-02 and 2024-03, through `record_support_eligible_spend`.
3. Call ListBusinessSupportAccountCharges(billingMonth="2024-03", maxResults=1) and keep the returned `nextToken`.
4. Call ListBusinessSupportAccountCharges(billingMonth="2024-02", nextToken=<that token>).
5. Observe HTTP 200 with `accountCharges: []` and `nextToken` absent, while `accountCount` and `totalSupportCharge` in the same body report February's full set — instead of the modeled ValidationException for a token that is not a cut point of this request's sequence.
6. Repeat step 4 with billingMonth="2024-04" (seeded) and observe the token silently ignored: the page restarts at the first account of that month.
