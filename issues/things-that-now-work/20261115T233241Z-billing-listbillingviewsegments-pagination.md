Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 41fa950ddd6915a1c40615232d76af513e3c005f

# A segment nextToken is not bound to the billing view it was minted for, unlike the crate's other per-view paginated read

_Filed: 2026-11-15T23:32:41Z_
_Operation: ListBillingViewSegments | Class: pagination_

> _Note: this issue was identified against the code base at commit `41fa950ddd6915a1c40615232d76af513e3c005f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListBillingViewSegments` pages one billing view's timeline. The store key it uses as a cursor is `"<viewArn>\u{1f}<zero-padded begin instant>"`, but the token it mints carries only the read's own name plus that raw key, and the decoded cursor is compared as a plain string against the keys of whatever view the next request names (`arn`, or the caller's PRIMARY view when `arn` is omitted). The token is also decoded BEFORE the view is resolved, so nothing ever compares the ARN inside the cursor with the ARN being walked.

Consequently a token minted while listing view A and replayed with `arn` naming view B is honoured: if A's ARN sorts above B's, every key of B fails `key.as_str() > after` and the read answers HTTP 200 with `items: []` and no `nextToken` — B's whole timeline silently dropped; if A's ARN sorts below B's, the cursor passes every key and the walk restarts at B's oldest segment. The same holds for a token replayed with a different `timeRange`, which is the other member that narrows the sequence (only segments overlapping the requested period are walked).

The crate already implements the opposite rule in its other per-view paginated read: `ListSourceViewsForBillingView` carries the view's ARN in the token's payload and refuses one "issued for another billing view". This read, whose cursor already CONTAINS the ARN, never checks it.

## Evidence

The token is decoded before the view is resolved, and the payload is treated as an opaque cursor — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_view_segments.rs:367-377:
```
        // The token is read back before the view is resolved, so the order the
        // refusals come in is the one this read already had; the key it is
        // authenticated under lives on the store, so the store is reached
        // first.
        let page_key = state.page_token_key();
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(decode_page_token(&page_key, Self::NAME, token)?),
            None => None,
        };

        let view = resolve_view(&input, &state)?;
```
...the resume comparison against another view's keys — list_billing_view_segments.rs:399-408:
```
        let mut matched: Vec<(&String, &crate::state::BillingViewSegment)> = state
            .billing_view_segments_of(&view.arn)
            .filter(|(key, _)| {
                resume_after
                    .as_deref()
                    .is_none_or(|after| key.as_str() > after)
            })
```
...and the mint of that same key — list_billing_view_segments.rs:420-424:
```
        let next_token = if matched.len() > limit {
            matched.truncate(limit);
            matched.last().map(|(key, _)| {
                crate::state::encode_page_token(&page_key, Self::NAME, key)
```
The key whose first segment IS the view ARN — services/service-com-amazonaws-billing-awsbilling/src/state/billing_view_segments.rs:109-115:
```
    pub fn key(&self) -> String {
        let began = self
            .begin_date_inclusive
            .map(|at| epoch_secs(at).max(0.0) as u64)
            .unwrap_or_default();
        format!("{}\u{1f}{began:0>20}", self.billing_view_arn)
    }
```
The crate's shared codec binds only the read's name — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1708-1712:
```
pub fn decode_page_token(key: &PageTokenKey, read_by: &str, token: &str) -> Option<String> {
    let cursor = key.cursor_of(&::hex::decode(token).ok()?)?;
    let (minted_by, payload) = cursor.split_once(PAGE_TOKEN_SCOPE_SEPARATOR)?;
    (minted_by == read_by).then(|| payload.to_string())
}
```
The sibling per-view read, which binds and checks the ARN — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:189-201 and 271-276:
```
/// The `nextToken` for a page of the view `arn` after which the walk has
/// reported `reported` ...
/// Both fields travel in the payload, because both decide what the next page
/// carries: the ARN names the SEQUENCE being walked, so a token minted while
/// walking one view is not a cut point in another's list
...
    if stored_arn != arn {
        return Err(invalid_field(
            "nextToken",
            "the pagination token was issued for another billing view",
        ));
    }
```
Test coverage: tests/all/token_scope.rs:6-12 drives only "a paginated read resumes a walk only from a `nextToken` THAT READ issued, and refuses every other token" — a segment token replayed against another view (or another `timeRange`) is the read's OWN token and is not covered.

## Steps to Reproduce

1. Boot the emulator and seed two segments on the caller's PRIMARY billing view plus two on a second view of the same account whose ARN sorts BELOW the PRIMARY view's (e.g. `billingview/custom-1` vs `billingview/primary`), through `record_billing_view_segment`.
2. Call ListBillingViewSegments(arn=<PRIMARY ARN>, timeRange=<the seeded period>, maxResults=1) and keep the `nextToken`.
3. Call ListBillingViewSegments(arn=<the lower-sorting view's ARN>, timeRange=<the same period>, nextToken=<that token>).
4. Observe HTTP 200 with `items: []` and no `nextToken` — that view's segments silently dropped — instead of the modeled ValidationException the sibling ListSourceViewsForBillingView answers for a token "issued for another billing view".
5. Repeat with the two ARNs the other way round and observe the opposite failure: the cursor is ignored and the page restarts at the view's oldest segment.
