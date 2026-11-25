Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 0c5548d176139ac2898f8496ebd5636c0a57c822

# The nextToken carries only the last ARN, scoped to no filter, so a token replayed with another arns/names/type filter is honoured as a cut point and matching views are silently dropped

_Filed: 2026-11-16T05:48:41Z_
_Operation: ListBillingViews | Class: pagination_

> _Note: this issue was identified against the code base at commit `0c5548d176139ac2898f8496ebd5636c0a57c822`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListBillingViews` walks `billing_views` (a BTreeMap keyed by ARN) and mints its `nextToken` from the ARN of the last view the page reported. The token's payload is that ARN and nothing else: the shared codec binds only the NAME of the read that minted it, so `decode_page_token` accepts the token from any later `ListBillingViews` request whatever `arns`, `names`, `billingViewTypes`, `ownerAccountId`, `sourceAccountId` or `activeTimeRange` that request carries. Every one of those members NARROWS the sequence being walked, so the cursor is a position in a different sequence than the one the next request asks about, and the handler applies it anyway: the walk first skips every view whose ARN is `<= after` and only then applies the new filters.

Two observable consequences, both answered HTTP 200 with no indication that anything was skipped:

* A token minted by an unfiltered (or differently filtered) page, replayed with a filter that selects a view whose ARN sorts at or below the cursor, drops that view from the answer entirely. `arns` is the sharpest case: `resolve_arns_filter` confirms the named ARN exists (so the request is not refused), and the walk then skips it because its ARN is below the cursor — the read answers `billingViews: []` with `nextToken` absent for a request naming exactly one view that this same store resolves and that `GetBillingView` returns 200 for.
* A token minted under a narrow filter, replayed with the filter removed, silently omits every view with an ARN at or below the cursor from the "widened" walk, and the final page carries no `nextToken`, so the caller reads a complete-looking list that is missing entries.

The crate already states and implements the opposite rule for the two reads whose sequence is likewise selected by request members: `GetBillingPreferences` digests its `filters` into a scope that travels in every token and refuses a token "issued for another set of filters", and `ListSourceViewsForBillingView` refuses one "issued for another billing view". `ListBillingViews` — the read with the most narrowing members of any paginated read in this service — binds nothing but the operation name.

## Evidence

The cursor is the bare ARN, decoded and applied before the filters — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_views.rs:408-432:
```
        // The page resumes strictly after the last ARN the previous page
        // returned, so a view inserted or removed between pages cannot make the
        // walk skip or repeat another one.
        // The key the store's tokens are authenticated under, taken off the
        // store by value so the walk below can borrow the views it reports.
        let page_key = state.page_token_key();
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(decode_page_token(&page_key, Self::NAME, token)?),
            None => None,
        };

        let mut matched: Vec<&crate::state::BillingView> = Vec::new();
        // `billing_views` is a BTreeMap keyed by ARN, so this walk is in ARN
        // order and the page boundary is stable.
        for view in state.billing_views.values() {
            if resume_after
                .as_deref()
                .is_some_and(|after| view.arn.as_str() <= after)
            {
                continue;
            }
            if matches_filters(view, &input)? {
                matched.push(view);
            }
        }
```
...and the mint, which carries only that ARN — list_billing_views.rs:442-449:
```
        let next_token = if matched.len() > limit {
            matched.truncate(limit);
            matched
                .last()
                .map(|view| crate::state::encode_page_token(&page_key, Self::NAME, &view.arn))
        } else {
            None
        };
```
The handler's own token doc enumerates the three tokens it refuses, and a token this read minted under OTHER filters is not among them — list_billing_views.rs:267-272:
```
/// The ARN a `nextToken` resumes after, or a `ValidationException` when the
/// token is not one THIS read issued under this store's key — one this service
/// cannot read back, one a caller assembled, and one it minted for another of
/// its paginated reads, whose payload is a cut point of another sequence and
/// names no view in this one. The token format and the refusal's wording live
/// in `crate::state`, shared with every other paginated read.
```
The shared codec binds ONLY the read's name — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1708-1712:
```
pub fn decode_page_token(key: &PageTokenKey, read_by: &str, token: &str) -> Option<String> {
    let cursor = key.cursor_of(&::hex::decode(token).ok()?)?;
    let (minted_by, payload) = cursor.split_once(PAGE_TOKEN_SCOPE_SEPARATOR)?;
    (minted_by == read_by).then(|| payload.to_string())
}
```
The filters that narrow the sequence and are applied fresh on every page — list_billing_views.rs:293-301 (`matches_filters`, `arns`):
```
fn matches_filters(
    view: &crate::state::BillingView,
    input: &ListBillingViewsInput,
) -> Result<bool, ListBillingViewsError> {
    if let Some(arns) = &input.arns {
        if !arns.is_empty() && !arns.contains(&view.arn) {
            return Ok(false);
        }
    }
```
...while an `arns` entry that exists is explicitly NOT refused — list_billing_views.rs:211-212:
```
    for arn in arns {
        if !state.billing_views.contains_key(arn) {
```
The rule the crate states for a read whose sequence is narrowed by request members — services/service-com-amazonaws-billing-awsbilling/src/ops/get_billing_preferences.rs:323-332:
```
/// The scope of the SEQUENCE this request pages through, which travels in every
/// `nextToken` the walk hands back.
///
/// A cursor is a position among the entries the request selects, so a token is
/// usable only by a request that selects the same ones: the scope covers every
/// member that narrows the sequence — here the filtered keys, digested in one
/// canonical order so two requests naming the same keys in different orders
/// share a scope. `maxResults` is deliberately NOT part of it: a page size is
/// how much of one walk a caller takes, not which entries the walk is over, so
/// a caller may change it between pages.
```
...and the refusal it mints — get_billing_preferences.rs:254-259:
```
    if stored_scope != scope {
        return Err(invalid_field(
            "nextToken",
            "the pagination token was issued for another set of filters",
        ));
    }
```
...the same rule in the crate's other per-sequence read — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:271-276:
```
    if stored_arn != arn {
        return Err(invalid_field(
            "nextToken",
            "the pagination token was issued for another billing view",
        ));
    }
```
Test coverage: tests/all/billing_views.rs:635-694 (`pagination_returns_every_view_exactly_once_for_every_page_size`) walks every page with the SAME request body, so no row ever changes a filter between pages; and tests/all/token_scope.rs:6-12 states the only rule its table drives — "a paginated read resumes a walk only from a `nextToken` THAT READ issued, and refuses every other token — one this service cannot read back at all, and one another of its reads minted" — so no row feeds `ListBillingViews` its OWN token under different filters.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs).
2. Create three custom billing views with CreateBillingView; they are minted as `.../billingview/custom-1`, `custom-2`, `custom-3`, which is also their ARN (walk) order.
3. Call ListBillingViews with `{"maxResults": 1}` and keep the returned `nextToken` (the page reports custom-1, so the cursor is custom-1's ARN).
4. Call ListBillingViews with `{"nextToken": <that token>, "arns": ["<custom-1 ARN>"]}`.
5. Observe HTTP 200 with `billingViews: []` and no `nextToken` — the one view the request names is silently skipped because its ARN sorts at or below the cursor — even though the same ARN resolves through GetBillingView (200) and through this read's own `arns` existence check.
6. Repeat step 4 with `{"nextToken": <that token>, "names": [{"searchOption": "STARTS_WITH", "searchValue": "<the name given to custom-1>"}]}` and observe the same empty, token-less page instead of the modeled ValidationException for a token that is not a cut point of this request's sequence (compare GetBillingPreferences, which refuses a token "issued for another set of filters").
