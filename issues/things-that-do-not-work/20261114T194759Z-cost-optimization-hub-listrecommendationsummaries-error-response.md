Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 96ace477b740a9765fa71f2821c079fa2ac6046e

# A blank groupBy is refused with a message naming no dimension, skipping the unspecified-dimension check both siblings apply

_Filed: 2026-11-14T19:47:59Z_
_Operation: ListRecommendationSummaries | Class: error-response_

> _Note: this issue was identified against the code base at commit `96ace477b740a9765fa71f2821c079fa2ac6046e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListRecommendationSummaries` resolves `groupBy` by handing the raw value straight to `parse_group_dimension` and, on `None`, building the refusal by splicing that raw value into the message. For an empty or whitespace-only `groupBy` — which the model admits, since the member is a plain `String` with `@required` and no `@length` or `@pattern`, so presence alone satisfies the pre-dispatch walk — the refusal degenerates into a sentence and a field entry that name no dimension at all: `"Invalid groupBy specified. Cannot group by  for this operation."` and `" is not a dimension recommendations can be grouped by"`.

Both sibling handlers in this crate apply exactly the check this one skips. `ListEfficiencyMetrics` tests `group_by.trim().is_empty()` BEFORE parsing and answers a dedicated refusal that states the rule ("Cannot group by an unspecified dimension for this operation."), and `ListRecommendations` does the same for `orderBy.dimension`. So the one refusal class the whole service words the same way — a dimension member that names nothing — is worded by two handlers and left degenerate by the third, even though this service's refusal contract requires the message to "name the rejected member and the rule that rejects it".

## Evidence

The path that skips the check — `services/service-com-amazonaws-costoptimizationhub-costoptimizationhubservice/src/ops/list_recommendation_summaries.rs:210-213`:
```
        let dimension = match crate::state::parse_group_dimension(&input.group_by) {
            Some(dimension) => dimension,
            None => return Err(unknown_group_by(&input.group_by)),
        };
```
and the message it produces — `src/ops/list_recommendation_summaries.rs:470-476`:
```
fn unknown_group_by(group_by: &str) -> ListRecommendationSummariesError {
    stated_refusal(
        format!("Invalid groupBy specified. Cannot group by {group_by} for this operation."),
        "groupBy",
        format!("{group_by} is not a dimension recommendations can be grouped by"),
    )
}
```
(with `group_by` = `""`, the message reads "Cannot group by  for this operation.")

The sibling that applies the check — `src/ops/list_efficiency_metrics.rs:264-271`:
```
        let dimension = match &input.group_by {
            None => None,
            Some(group_by) if group_by.trim().is_empty() => return Err(no_group_dimension()),
            Some(group_by) => match crate::state::parse_group_dimension(group_by) {
                Some(dimension) => Some(dimension),
                None => return Err(unknown_group_by(group_by)),
            },
        };
```
with its dedicated refusal — `src/ops/list_efficiency_metrics.rs:953-962`:
```
fn no_group_dimension() -> ListEfficiencyMetricsError {
    stated_refusal(
        "Invalid groupBy specified. Cannot group by an unspecified dimension for this operation."
            .to_string(),
```
and the same treatment for the other dimension member — `src/ops/list_recommendations.rs:199-202`:
```
                let dimension = order_by.dimension.as_deref().unwrap_or_default();
                if dimension.trim().is_empty() {
                    return Err(no_dimension());
                }
```

The contract the degenerate message breaks — `src/service.rs:322-326`: "This service's `ValidationException` for a request a HANDLER refused for a rule of the OPERATION the model cannot express: `statement` is the top-level `message`, which names the rejected member and the rule that rejects it because the refusal is built from the request".

The member really is unconstrained — `smithy_query(query="operation", operation="ListRecommendationSummaries")`: `"groupBy": {"target": "String", "required": true}` (no length, no pattern), so `""` reaches the handler.

Test coverage: `tests/all/summary_metrics_and_refusals.rs:451-465` exercises only non-blank unknown values (`"NotADimension"`, `"ResourceId"`) for this operation, while `tests/all/efficiency_metrics.rs:940-942` pins the blank case for the sibling — so nothing pins the blank `groupBy` here.

## Steps to Reproduce

1. POST `X-Amz-Target: CostOptimizationHubService.ListRecommendationSummaries` with body `{"groupBy": ""}` (or `{"groupBy": " "}`).
2. Observe HTTP 400 `ValidationException` whose `message` is "Invalid groupBy specified. Cannot group by  for this operation." and whose single `fields` entry message is " is not a dimension recommendations can be grouped by" — neither names a dimension or the rule.
3. POST `CostOptimizationHubService.ListEfficiencyMetrics` with `{"granularity": "Monthly", "timePeriod": {...}, "groupBy": ""}` and observe the sibling's refusal: "Invalid groupBy specified. Cannot group by an unspecified dimension for this operation."
4. Compare `src/ops/list_recommendation_summaries.rs:210-213` (no blank branch) with `src/ops/list_efficiency_metrics.rs:264-271` and `src/ops/list_recommendations.rs:199-202` (both have one).
