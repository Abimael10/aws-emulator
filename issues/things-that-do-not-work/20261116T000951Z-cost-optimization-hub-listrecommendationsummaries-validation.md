Issue-Severity: LOW
Issue-Gap-Class: validation
Issue-Commit: 83b924bfca64d21d2064b8fa56823232662207f8

# ListRecommendationSummaries skips the blank-`groupBy` check its sibling ListEfficiencyMetrics applies

_Filed: 2026-11-16T00:09:51Z_
_Operation: ListRecommendationSummaries | Class: validation_

> _Note: this issue was identified against the code base at commit `83b924bfca64d21d2064b8fa56823232662207f8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both grouped readers of this crate resolve `groupBy` through the same helper `crate::state::parse_group_dimension`, but only `ListEfficiencyMetrics` first checks that the value actually NAMES a dimension. `ListRecommendationSummaries` hands the raw value straight to the helper, so a `groupBy` that is empty or whitespace-only (input the model admits: `groupBy` is a plain `String`, `@required` only, with no `@length` or `@pattern`) falls into the "names no dimension a summary can be grouped by" arm and is refused with a sentence that interpolates the blank value, i.e. `"Invalid groupBy specified. Cannot group by  for this operation."` — a refusal that states no rejected value at all. The sibling handler has a dedicated branch and refusal for exactly this input class; this handler skips it, so the one rule the two readers share is applied in only one of them.

## Evidence

Sibling applies the check — services/service-com-amazonaws-costoptimizationhub-costoptimizationhubservice/src/ops/list_efficiency_metrics.rs:267-274:
```rust
        let dimension = match &input.group_by {
            None => None,
            Some(group_by) if group_by.trim().is_empty() => return Err(no_group_dimension()),
            Some(group_by) => match crate::state::parse_group_dimension(group_by) {
                Some(dimension) => Some(dimension),
                None => return Err(unknown_group_by(group_by)),
            },
        };
```
with its own refusal at list_efficiency_metrics.rs:955-964: `"Invalid groupBy specified. Cannot group by an unspecified dimension for this operation."` and the field message `"a dimension cost efficiency metrics can be grouped by is required whenever groupBy is specified"`.

This handler skips it — services/.../src/ops/list_recommendation_summaries.rs:210-213:
```rust
        let dimension = match crate::state::parse_group_dimension(&input.group_by) {
            Some(dimension) => dimension,
            None => return Err(unknown_group_by(&input.group_by)),
        };
```
and the only refusal it can reach, list_recommendation_summaries.rs:470-476:
```rust
fn unknown_group_by(group_by: &str) -> ListRecommendationSummariesError {
    stated_refusal(
        format!("Invalid groupBy specified. Cannot group by {group_by} for this operation."),
```
For `group_by == ""` the formatted sentence contains no value.

Model (smithy_query operation=ListRecommendationSummaries): `"groupBy": {"target": "String", "required": true}` — no length/pattern constraint, so a blank value reaches the handler.

Test coverage: the blank form IS pinned for the sibling (tests/all/efficiency_metrics.rs:940-942, case `"an orderBy naming no dimension"` sits beside the groupBy table in the same suite), while tests/all/summary_metrics_and_refusals.rs:462-470 covers only non-blank unsupported `groupBy` values (`{"groupBy": "ResourceId"}`), so no test pins the blank case for this operation.

## Steps to Reproduce

1. POST / with X-Amz-Target: CostOptimizationHubService.ListRecommendationSummaries and body {"groupBy": ""}
2. Observe HTTP 400 ValidationException whose message is "Invalid groupBy specified. Cannot group by  for this operation." — the rejected value is absent from the sentence
3. POST / with X-Amz-Target: CostOptimizationHubService.ListEfficiencyMetrics and body {"granularity": "Monthly", "timePeriod": {...valid...}, "groupBy": ""}
4. Observe the sibling's dedicated refusal "Invalid groupBy specified. Cannot group by an unspecified dimension for this operation.", proving the check exists in the crate and is skipped by ListRecommendationSummaries
