Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 53eda64883178af14e234ca05ac2be08d2ce631b

# GetRecommendationSummaries ignores maxResults entirely: the whole summary set is returned on one page whatever size was asked for

_Filed: 2026-11-18T22:34:13Z_
_Operation: GetRecommendationSummaries | Class: pagination_

> _Note: this issue was identified against the code base at commit `53eda64883178af14e234ca05ac2be08d2ce631b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

GetRecommendationSummaries is modelled `@paginated(inputToken=nextToken, outputToken=nextToken, items=recommendationSummaries, pageSize=maxResults)`, and the member doc states what `maxResults` is for. The handler never reads `input.max_results`: it is the only paginated reader in the crate that calls neither `crate::state::page_size` nor `crate::state::offset_page`, it pushes one summary for every entry of `TRACKED_SOURCE_TYPES` (16 entries) and then hard-codes `next_token: None`.

So a client that asks for `maxResults: 1` is answered with all 16 summaries and no token, and a client that walks the operation the way the model's paginator does can never observe the bound it asked for. The handler still DECODES a `nextToken` and uses it to skip into the vocabulary, so the resume half of the cursor is implemented while the size half is dropped — an input token it can only ever receive from a client that forged one, since the operation hands none out.

## Evidence

Smithy, GetRecommendationSummariesRequest$maxResults (smithy_query query="doc"): "The maximum number of recommendation summaries to return with a single request." / "To retrieve the remaining results, make another request with the returned nextToken value." — and the operation carries `paginated: {inputToken: nextToken, outputToken: nextToken, items: recommendationSummaries, pageSize: maxResults}`.

The handler never consults the member — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_recommendation_summaries.rs:339-364:
```
        for resource_type in crate::state::TRACKED_SOURCE_TYPES
            .iter()
            .skip(resume_at)
            .copied()
        {
...
        Ok(GetRecommendationSummariesOutput {
            // The whole set is above, so there is no page to advance to: "This value is null
            // when there are no more pages of recommendation summaries to return."
            next_token: None,
```
(no `page_size` call anywhere in the file; `input.max_results` appears nowhere).

The crate's own rule for the two halves of paging — src/state.rs:11-14:
```
//! from each other's view of the same record. The two rules EVERY paginated reader of this
//! service applies live here for the same reason: how large a page a request asks for
//! ([`page_size`]) and which items that page carries, with the token that resumes the walk
//! after it ([`offset_page`]).
```
and src/state.rs:2256-2263: "How large a page a paginated reader of this service builds for a request. The rule every one of them follows, stated once ...". Every sibling reader applies it, e.g. src/ops/describe_recommendation_export_jobs.rs:231 and src/ops/get_recommendation_preferences.rs:227.

Existing coverage pins the current behaviour with a reason the model text contradicts — tests/all/recommendation_summaries.rs:580-583: "`maxResults` never changes WHAT is summarised: the summary set is bounded by the resource-type vocabulary, so every request is answered with the whole set in one answer, in the same order and with no `nextToken` — whatever size it asks for." The model says `maxResults` is the maximum number of summaries returned with a single request and that `nextToken` retrieves the remainder, so the vocabulary being finite (16 entries, more than any small `maxResults`) is not a reason for the bound not to apply.

## Steps to Reproduce

1. Opt the account in (UpdateEnrollmentStatus status=Active).
2. Call GetRecommendationSummaries with {"maxResults": 1}.
3. Observe `recommendationSummaries` carries all 16 tracked resource types rather than 1, and `nextToken` is null, so the remaining summaries can never be walked as the model's paginator expects.
4. Repeat with {"maxResults": 2}: the answer is byte-identical to the unsized one (tests/all/recommendation_summaries.rs:585 asserts exactly this).
