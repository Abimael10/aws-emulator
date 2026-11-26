Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 780566efa1a099e163927d46dd50b271247016ff

# The stored-resource projection is re-stated a third time inside ListRecommendationsForResource instead of going through state::resource_summary

_Filed: 2026-11-19T11:41:40Z_
_Operation: ListRecommendationsForResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `780566efa1a099e163927d46dd50b271247016ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::resource_summary` is declared by this crate to be the ONE projection of a stored `ResourceEntry`, and `crate::state::organization_resource_summary` consumes it by destructuring it by name so the organization view cannot drift from the account view. `ListRecommendationsForResource`'s row builder, `recommendation_for_resource_summary`, reports four of those very same members (`status`, `lastUpdatedAt`, `exclusionStatus`, `metadata`) and its own doc comment asserts they are "the same values `ListRecommendationResources` reports for it" — but the code computes them itself off the `ResourceEntry` (a second `status.clone()`, a second `date_time(resource.last_updated_at_millis)`, a second `exclusion_status.clone()` and a second hand-rolled metadata-map rebuild) instead of taking them from the shared projection. The two copies of that computation can therefore drift: a change to how a resource's `lastUpdatedAt`, `exclusionStatus` or `metadata` is projected in `state::resource_summary` leaves this third view reporting the old values while the comment above it still claims they are identical. The members' Rust types are the same on both shapes (`status: String`, `last_updated_at: ::aws_smithy_types::DateTime`, `exclusion_status: String`, `metadata: std::collections::HashMap<String, String>` in `list_recommendation_resources_types.rs:40-43` and `list_recommendations_for_resource_types.rs:39-42`), so this view can destructure `resource_summary(resource)` by name exactly as `organization_resource_summary` already does, leaving every response byte and every existing test unchanged.

## Evidence

The crate's own rule, services/service-com-amazonaws-trustedadvisor-trustedadvisor/src/state.rs:1041-1049:
```
/// This is the projection of a [`ResourceEntry`] that BOTH resource lists
/// report — `ListRecommendationResources` over one recommendation's resources
/// and `ListOrganizationRecommendationResources` over the member accounts'
/// resources an organization recommendation aggregates — so it is stated here
/// ONCE, as [`apply_lifecycle_update`] states the one lifecycle write of the two
/// levels. Reporting the two views from one projection is what makes them unable
/// to disagree about a resource, instead of agreeing only while two copies of
/// the projection happen to be written identically.
pub fn resource_summary(resource: &ResourceEntry) -> RecommendationResourceSummary {
```
with the one copy of the computation, src/state.rs:1055-1062:
```
        status: resource.status.clone(),
        metadata: resource
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        last_updated_at: date_time(resource.last_updated_at_millis),
        exclusion_status: resource.exclusion_status.clone(),
```
How the sibling view consumes it instead of re-stating it, src/state.rs:1087-1100:
```
    // Destructured by NAME, so every shared member reaches the organization
    // view as the projection computed it: the account-level
    // `recommendationArn` is the only one this view replaces outright.
    let RecommendationResourceSummary {
        id,
        arn,
        aws_resource_id,
        region_code,
        status,
        metadata,
        last_updated_at,
        exclusion_status,
        recommendation_arn: _,
    } = resource_summary(resource);
```
The second copy, src/ops/list_recommendations_for_resource.rs:135-137 (the claim) and 149-156 (the re-derivation):
```
/// `status`, `exclusionStatus`, `metadata` and `lastUpdatedAt` are the flagged
/// resource's own — the same values `ListRecommendationResources` reports for
/// it — while `pillars` is the recommendation's, since a pillar classifies the
```
```
        status: resource.status.clone(),
        last_updated_at: crate::state::date_time(resource.last_updated_at_millis),
        exclusion_status: resource.exclusion_status.clone(),
        metadata: resource
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
```
The member types that make the destructure drop-in, src/ops/types/list_recommendation_resources_types.rs:40-43 vs src/ops/types/list_recommendations_for_resource_types.rs:39-42:
```
    pub status: String,
    pub metadata: std::collections::HashMap<String, String>,
    pub last_updated_at: ::aws_smithy_types::DateTime,
    pub exclusion_status: String,
```
```
    pub status: String,
    pub last_updated_at: ::aws_smithy_types::DateTime,
    pub exclusion_status: String,
    pub metadata: std::collections::HashMap<String, String>,
```
Test coverage that must stay green and unchanged: tests/all/recommendations_for_resource.rs:1-14 pins the whole wire contract of this operation ("the flagged resource supplying `awsResourceArn`, `status`, `exclusionStatus`, `metadata` and `lastUpdatedAt`"), and tests/all/shared_resource_view.rs:1-19 pins the shared projection for the two resource lists (SHARED_MEMBERS at tests/all/shared_resource_view.rs:54-63).

## Steps to Reproduce

1. Read src/state.rs:1041-1049: the crate states that the projection of a stored `ResourceEntry` is stated ONCE, precisely so two copies cannot drift.
2. Read src/state.rs:1087-1100: the organization-level view obtains `status`, `metadata`, `last_updated_at` and `exclusion_status` by destructuring `resource_summary(resource)` by name.
3. Read src/ops/list_recommendations_for_resource.rs:149-156: the third view of the same stored resource recomputes those four members off the `ResourceEntry` instead of calling `resource_summary`, while its own doc comment at lines 135-137 asserts the values are the same ones `ListRecommendationResources` reports.
4. Compare src/ops/types/list_recommendation_resources_types.rs:40-43 with src/ops/types/list_recommendations_for_resource_types.rs:39-42: the four members have identical Rust types, so the destructure used by the organization view applies verbatim here and no response value changes.
5. Confirm the behaviour is pinned both ways: tests/all/recommendations_for_resource.rs (this operation's wire contract) and tests/all/shared_resource_view.rs (the shared projection) must both still pass unchanged after the duplicate computation is removed.
