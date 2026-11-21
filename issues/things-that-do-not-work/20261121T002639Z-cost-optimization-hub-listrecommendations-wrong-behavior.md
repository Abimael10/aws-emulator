Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9fc11ee3a6ed8321eab8f29455b82e2f328ae1e5

# orderBy.dimension naming currentResourceSummary or recommendedResourceSummary is refused although both are Recommendation members the sortable set is defined to hold

_Filed: 2026-11-21T00:26:39Z_
_Operation: ListRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9fc11ee3a6ed8321eab8f29455b82e2f328ae1e5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The sortable-dimension set this handler resolves `orderBy.dimension` through is DEFINED in the crate as "every `Recommendation` member that carries an ordered value", with exactly one stated exclusion (the three mode-dependent money members, which `Dimension::parse` resolves separately). Two `Recommendation` members that carry an ordered scalar value — `currentResourceSummary` and `recommendedResourceSummary`, both plain `String` in the model and both already stored on `StoredRecommendation` and rendered into every list row — have no arm in `Dimension` / `Dimension::parse` / `dimension_key`, and no comment excludes them.

The consequence: `ListRecommendations` with `orderBy = {"dimension": "currentResourceSummary"}` falls into the `None` arm of the parse and is answered HTTP 400 `ValidationException` whose field message asserts `"currentResourceSummary is not a dimension recommendations can be sorted by"` — a statement the crate's own definition of that set contradicts, and which the handler's module doc contradicts too (it says only a dimension that "names no `Recommendation` member" is refused). The values are already in hand: nothing new has to be computed, only the two missing match arms of an existing helper. `tags` is the only other unmapped member and is the one member with no ordered value (a list), so the gap is specifically these two strings.

## Evidence

The rule the set is defined by — src/state.rs:625-630:
```
/// A dimension `ListRecommendations`' `orderBy` can sort on. Each one is a
/// `Recommendation` member that carries an ordered value, named as the model
/// names it (`smithy_query query="shape" shape="Recommendation"`); the two
/// mode-dependent money members are not here because the value they carry
/// depends on the caller's stored `savingsEstimationMode`, so
/// [`Dimension::parse`] resolves them in the handler instead.
```
The set itself — src/state.rs:631-648 (`pub enum Dimension`) lists RecommendationId, AccountId, Region, ResourceId, ResourceArn, CurrentResourceType, RecommendedResourceType, ActionType, ImplementationEffort, Source, CurrencyCode, RestartNeeded, RollbackPossible, RecommendationLookbackPeriodInDays, LastRefreshTimestamp — and `Dimension::parse` (src/state.rs:691-725) has arms only for those plus the three money members:
```
            "lastrefreshtimestamp" => Self::LastRefreshTimestamp,
            "estimatedmonthlycost" => { return Some(OrderDimension::Money(MoneyDimension::EstimatedMonthlyCost)) }
...
            _ => return None,
```
No arm for `currentresourcesummary` / `recommendedresourcesummary`.

The model declares both as ordered scalars — smithy_query(query="shape", shape="Recommendation"): `"currentResourceSummary": {"target": "String", "doc": "<p>Describes the current resource.</p>"}`, `"recommendedResourceSummary": {"target": "String", "doc": "<p>Describes the recommended resource.</p>"}`.

The handler's own stated rule — src/ops/list_recommendations.rs:23-27:
```
//! An `orderBy` the request sets is honoured as a whole: its `dimension` says
//! which values the rows are put in order of, so one that names no
//! `Recommendation` member and one that carries no `dimension` at all are both
//! refused with the modelled `ValidationException` instead of being served in an
//! order the caller did not ask for.
```

The values exist in the crate already — src/state.rs:447-448 (`pub current_resource_summary: String,` / `pub recommended_resource_summary: String,`) and src/ops/list_recommendations.rs:331-332 (`current_resource_summary: Some(row.current_resource_summary.clone()), recommended_resource_summary: Some(row.recommended_resource_summary.clone()),`), so every row the list answers already carries the value a sort would use.

The refusal the caller gets instead — src/ops/list_recommendations.rs:352-361:
```
fn unknown_dimension(dimension: &str) -> ListRecommendationsError {
    stated_refusal(
        format!(
            "Invalid {ORDER_BY} specified. Cannot {ORDER_ACTION} by {dimension} for this \
             operation."
        ),
        format!("{dimension} is not {ORDER_DIMENSIONS}"),
    )
}
```

Test coverage: tests/all/order_by_fidelity.rs:110-148 (`an_order_by_dimension_naming_no_member_is_refused`) pins the refusal only for values that are NOT ordered `Recommendation` scalars — `"resourceTypes"`, `"savingsPercentage"`, `"estimated_monthly_savings"`, `"tags"` (the list member), `"order"` — and tests/all/order_by_fidelity.rs:158-195 pins the served arm for `"recommendationId"`, `"actionType"`, `"recommendationLookbackPeriodInDays"`, `"estimatedMonthlySavings"`. No test names either summary member (grep for `currentResourceSummary` under tests/ returns no match), so nothing pins the current refusal.

## Steps to Reproduce

1. Enroll the caller: POST / with X-Amz-Target: CostOptimizationHubService.UpdateEnrollmentStatus and body {"status": "Active"}, then wait until ListRecommendations answers rows
2. POST / with X-Amz-Target: CostOptimizationHubService.ListRecommendations and body {"includeAllRecommendations": true, "orderBy": {"dimension": "currentResourceSummary", "order": "Asc"}}
3. Observe HTTP 400 ValidationException, message "Invalid orderBy specified. Cannot order by currentResourceSummary for this operation." and the fields entry "currentResourceSummary is not a dimension recommendations can be sorted by"
4. Compare with the same request using "dimension": "currentResourceType" — served HTTP 200, ordered — and with src/state.rs:625-630, which defines the set as every Recommendation member carrying an ordered value, and src/ops/list_recommendations.rs:331 which already emits currentResourceSummary for every row
5. Repeat with "dimension": "recommendedResourceSummary" for the same refusal
