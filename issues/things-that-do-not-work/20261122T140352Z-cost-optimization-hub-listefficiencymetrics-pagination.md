Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 49dcb0a50d4414692f7327267e9f9393b9923fcd

# maxResults=0 is answered with an empty page and NO nextToken, so the groups that exist become unreachable

_Filed: 2026-11-22T14:03:52Z_
_Operation: ListEfficiencyMetrics | Class: pagination_

> _Note: this issue was identified against the code base at commit `49dcb0a50d4414692f7327267e9f9393b9923fcd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`maxResults` is a value the model explicitly admits down to zero (`range {min: 0, max: 1000}`, and the member's own doc says "Valid values range from 0 to 1000"). The shared page helper every paginated read of this service goes through turns a zero page size into an empty page AND suppresses the continuation token, because its only "no token" test is `items.is_empty() || end >= all.len()`.

So `ListEfficiencyMetrics{granularity, timePeriod, maxResults: 0}` for an account that HAS groups answers HTTP 200 with `efficiencyMetricsByGroup: []` and `nextToken: null` — a dead end: the caller is told nothing is available and is handed no token to continue with, although every group is still there (the same call without `maxResults` returns them). That contradicts the rule the input member itself states — the token "is returned in the response when the number of groups exceeds the specified maxResults value" (1 group exceeds 0) — and the output member's "When this value is present in the response, additional groups are available", which the caller reads in reverse as "nothing more is available".

It also breaks the premise the crate's own token doc relies on: `issued_offset` justifies the issued-offset set with "it closes a page carrying at least one row", which is exactly what the zero-size page does not do. The same helper serves `ListRecommendations`, `ListRecommendationSummaries` and `ListEnrollmentStatuses`, so all four paginated reads share this dead end; `ListEfficiencyMetrics` is the one whose member documents the rule verbatim. Nothing in the crate justifies the zero case: the only comment about a missing token is "The token is issued only when rows remain after this page, so the last page carries none" (state.rs:1000-1002) — here rows DO remain, and this is not the last page.

## Evidence

Model, `smithy_query(query="doc", shape="ListEfficiencyMetricsRequest", member="nextToken")`:
> "The token to retrieve the next page of results. This value is returned in the response when the number of groups exceeds the specified maxResults value."

Model, `smithy_query(query="doc", shape="ListEfficiencyMetricsResponse", member="nextToken")`:
> "The token to retrieve the next page of results. When this value is present in the response, additional groups are available. Pass this token in the nextToken parameter of a subsequent request to retrieve the next page."

Model, `smithy_query(query="operation", operation="ListEfficiencyMetrics")` — the member admits zero: `"maxResults": {"target": "MaxResults", "range": {"min": 0, "max": 1000}}`, and its doc: "The maximum number of groups to return in the response. Valid values range from 0 to 1000."

The code that drops the token — `src/state.rs:1013-1021`:
```
        let remaining = &all[start..];
        let limit = max_results.map_or(remaining.len(), |m| usize::try_from(m).unwrap_or(0));
        let items = &remaining[..limit.min(remaining.len())];
        let end = start + items.len();
        let next_token = if items.is_empty() || end >= all.len() {
            None
        } else {
            Some(encode_page_token(kind, end))
        };
```
With `max_results = Some(0)`: `limit = 0`, `items = []`, `end = 0`, and `items.is_empty()` short-circuits to `None` even though `all.len() > 0`.

The crate's own doc whose premise this path breaks — `src/state.rs:980-984`:
```
/// [`paginate`] issues a token only when rows remain after the page it closes,
/// and it closes a page carrying at least one row, so every offset this service
/// hands out for a list of `len` rows points at a row of that list and never at
/// its first: the issued offsets are `1..len`.
```
and the only comment about an absent token — `src/state.rs:1000-1002`:
```
/// `max_results` caps the page; absent, the whole remainder is returned. The
/// token is issued only when rows remain after this page, so the last page
/// carries none.
```

The handler that reaches it — `src/ops/list_efficiency_metrics.rs:333-344`:
```
        // The page this read answers, under the one pagination-token rule every
        // paginated read of this service applies (`crate::service::page`). The
        // page is of GROUPS — which is what `maxResults` bounds and `nextToken`
        // resumes — so every group it carries reports the whole series.
        let page = crate::service::page(
            Self::NAME,
            PAGE_KIND,
            &groups,
            input.next_token.as_deref(),
            input.max_results,
        )
```

Test coverage: `tests/all/max_results_bounds.rs:92-106` sends `maxResults = op.min` (i.e. 0) for all four paginated operations but asserts only `status == 200` and `payload["fields"].is_null()` — neither the emptied `efficiencyMetricsByGroup` nor the missing `nextToken` is pinned, so the zero-page behaviour is unpinned by any test.

## Steps to Reproduce

1. Boot the emulator; call Compute Optimizer `UpdateEnrollmentStatus{status: "Active"}` so the Compute Optimizer opt-in gate in `src/ops/list_efficiency_metrics.rs:303-311` passes.
2. Call Cost Optimization Hub `UpdateEnrollmentStatus{status: "Active"}` and wait out `RECOMMENDATION_GENERATION_DELAY` (src/state.rs:100) so the seeded catalog is readable.
3. POST `X-Amz-Target: CostOptimizationHubService.ListEfficiencyMetrics` with `{"granularity": "Monthly", "timePeriod": {"start": "<last month>", "end": "<this month>"}}` (no maxResults) and note that `efficiencyMetricsByGroup` carries a group with a series.
4. Repeat the same call with `"maxResults": 0` — a value the member's `@range` and its doc both admit.
5. Observe HTTP 200 with `efficiencyMetricsByGroup: []` and `nextToken` absent: the group that exists is neither returned nor resumable, although the input member documents that a token "is returned in the response when the number of groups exceeds the specified maxResults value".
6. The same dead end reproduces through the shared helper on `ListRecommendations{"includeAllRecommendations": true, "maxResults": 0}` and `ListEnrollmentStatuses{"includeOrganizationInfo": false, "maxResults": 0}`.
