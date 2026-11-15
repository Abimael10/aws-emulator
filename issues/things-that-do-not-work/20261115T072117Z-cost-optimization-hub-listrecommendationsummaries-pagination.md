Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: cfc4a64aa1fece035f2324d3ed7346f0c5cffa22

# A nextToken is scoped only to the operation, not to the request that minted it, so a token from one groupBy resumes a different grouping mid-list

_Filed: 2026-11-15T07:21:17Z_
_Operation: ListRecommendationSummaries | Class: pagination_

> _Note: this issue was identified against the code base at commit `cfc4a64aa1fece035f2324d3ed7346f0c5cffa22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated read of this service goes through one token helper, and the token body carries only `version:kind:offset` — `kind` being a per-OPERATION constant (`PAGE_KIND`). Nothing in the token, and nothing in the check, ties the token to the request members that decide WHICH list the operation is paging: `groupBy` for `ListRecommendationSummaries` (a @required member that fully determines the set of groups), `filter`/`orderBy` for `ListRecommendations`, `groupBy`/`granularity`/`timePeriod` for `ListEfficiencyMetrics`.

The only thing the acceptance check asks is whether the offset is in `1..len` of the list being answered NOW (`issued_offset`). So a token minted while paging one grouping is accepted when presented with a different `groupBy`, as long as the offset happens to fall inside the new, differently-sized list — and the read is then served as a normal page that starts in the middle of a list no page of which ever ended at that offset. The caller silently never sees the skipped groups and gets no `nextToken` telling it anything is missing, i.e. an invalid request is answered as a listing.

With the seeded catalog this is reachable in two calls: `groupBy=ActionType` yields 4 groups (Rightsize, Upgrade, PurchaseSavingsPlans, Delete) and `groupBy=Region` yields 2 (us-east-1 = 588.16, us-west-2 = 262.55). A token issued by the ActionType page (offset 1) presented to a Region read passes `0 < 1 < 2` and the service answers one group — us-west-2 — dropping us-east-1 and its 588.16 of savings, with `nextToken` null so the walk looks complete.

This contradicts the rule the crate states for the helper in three places (quoted below), and it is not what any existing test pins: the suite's cross-list cases are all cases where the offset is out of range for the narrower list, which is why they are refused.

## Evidence

The rule the crate states — `src/state.rs:970-973` (the doc on `TokenNotIssued`):
```
/// That a `nextToken` is not one this service issued for the list being read, so
/// it names no row to resume at: either it is not in the encoding
/// [`encode_page_token`] writes for that list, or it carries an offset no page of
/// that list ever ended at.
```
and `src/service.rs:457-463` (the doc on the one entry point every handler calls):
```
/// The page a paginated read of `kind` answers out of `all`, for every paginated
/// operation of this service: no token asks for the first page, a token this
/// service issued for that list resumes where that list's previous page stopped
/// ([`crate::state::paginate`]), and anything else is refused, because a token
/// this list did not issue — one the service cannot read, or one carrying an
/// offset no page of the list ever ended at — names no row to resume at, and
/// answering it with a page would serve an invalid request as a listing.
```

What the token actually carries — `src/state.rs:942-948`:
```
/// `kind` names the list the token belongs to, so a token issued by one
/// paginated operation is not accepted by another.
pub fn encode_page_token(kind: &str, offset: usize) -> String {
    ::hex::encode(format!("{PAGE_TOKEN_VERSION}:{kind}:{offset}"))
}
```

The whole acceptance check — `src/state.rs:985-991`:
```
fn issued_offset(kind: &str, len: usize, token: &str) -> Result<usize, TokenNotIssued> {
    let offset = decode_page_token(kind, token).ok_or(TokenNotIssued)?;
    if offset == 0 || offset >= len {
        return Err(TokenNotIssued);
    }
    Ok(offset)
}
```

The handler that pages a list whose identity is a request member — `src/ops/list_recommendation_summaries.rs:210-213` (the grouping dimension) and `:309-318` (the page), with `PAGE_KIND` fixed per operation at `:340-341`:
```
        let dimension = match crate::state::parse_group_dimension(&input.group_by) {
```
```
        // The page this read answers, under the one pagination-token rule every
        // paginated read of this service applies (`crate::service::page`).
        let page = crate::service::page(
            Self::NAME,
            PAGE_KIND,
            &groups,
            input.next_token.as_deref(),
            input.max_results,
        )
```
```
/// The list a `nextToken` from this operation belongs to.
const PAGE_KIND: &str = "recommendation-summaries";
```
So the token for `groupBy=ActionType` and for `groupBy=Region` are the SAME string for the same offset.

The groups the quoted reproduction relies on come from the seeded catalog, `src/state.rs:1474-1708` (7 rows, de-duped to 6 by `resource_id`): action types Rightsize/MigrateToGraviton/Upgrade/PurchaseSavingsPlans/Delete over regions `us-east-1` (`src/state.rs:1471`) and `us-west-2` (`:1472`).

Test coverage: `tests/all/page_token_rule.rs:7-11` states the rule under test ("a token that read's own list did not issue names no row to resume at, so it is refused with HTTP 400 `ValidationException` attributed to `nextToken`, rather than silently served"), and `:363-396` is the closest case — a token issued for a WIDER scope of the SAME operation, refused only because the offset is past the narrower list's single row (`:377-382` asserts that read answers one record). No case in the suite presents a token to the same operation with a different `groupBy`/`filter`, so the behaviour above is unpinned; a fix that scopes the token to the list identity keeps every refusal case in that file refused and every same-request walk (`:239-272`) unchanged.

## Steps to Reproduce

1. Boot the emulator, call `UpdateEnrollmentStatus` with `{"status": "Active"}` and wait out `RECOMMENDATION_GENERATION_DELAY` (src/state.rs:100) so the catalog is readable.
2. POST `X-Amz-Target: CostOptimizationHubService.ListRecommendationSummaries` with `{"groupBy": "ActionType", "maxResults": 1}`; keep the returned `nextToken` (it encodes `cohv1:recommendation-summaries:1`).
3. POST the same target with `{"groupBy": "Region", "nextToken": "<that token>"}`.
4. Observe HTTP 200 carrying a single group (`us-west-2`) and `nextToken: null`, instead of the `ValidationException` the quoted token rule owes: the Region listing's first group (`us-east-1`, the larger saving) is silently skipped and the caller believes it has paged the whole grouping.
5. The same holds for the sibling readers through the same helper: `ListRecommendations` `{"includeAllRecommendations": true, "maxResults": 1}` mints a token that is then accepted for `{"includeAllRecommendations": true, "filter": {"actionTypes": ["Upgrade"]}}` (2 rows), serving row 2 of that filtered list and skipping row 1.
