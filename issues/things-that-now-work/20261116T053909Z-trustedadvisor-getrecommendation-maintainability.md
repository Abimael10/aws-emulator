Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 5e7424483e940ccd55a87c264bc5455cd8b400c9

# The recommendation projection — including the Priority createdAt scoping and the language selection — is written out four times instead of once

_Filed: 2026-11-16T05:39:09Z_
_Operation: GetRecommendation | Class: maintainability_

> _Note: this issue was identified against the code base at commit `5e7424483e940ccd55a87c264bc5455cd8b400c9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states, as an explicit design rule, that a projection reported by two surfaces lives in `crate::state` and is stated ONCE, so the surfaces "cannot disagree ... instead of agreeing only while two copies of the projection happen to be written identically" (src/state.rs:796-803). It follows that rule for the RESOURCE projection (`resource_summary` / `organization_resource_summary`), for the lifecycle WRITE (`apply_lifecycle_update`), for the recommendation-listing FILTERS (`RecommendationFilters` + `RecommendationFilterable`) and for the resource filters (`ResourceFilters`) — each stated once in `crate::state` and reached from both levels.

The RECOMMENDATION projection is the one that was never moved. The same projection of a stored recommendation entry is written out FOUR times, once per handler:

* `src/ops/get_recommendation.rs:116-147` — `fn recommendation` -> `Recommendation`
* `src/ops/list_recommendations.rs:145-168` — `fn recommendation_summary` -> `RecommendationSummary`
* `src/ops/get_organization_recommendation.rs:98-129` — `fn organization_recommendation` -> `OrganizationRecommendation`
* `src/ops/list_organization_recommendations.rs:129-153` — `fn organization_recommendation_summary` -> `OrganizationRecommendationSummary`

The four are not merely parallel field copies: each one re-states the same RULES.

1. The Priority scoping of `createdAt` — `crate::state::priority_created_at(&entry.r#type, entry.created_at_millis)` — is called from all four, so the one helper the crate created to hold that rule is still invoked from four places that each decide for themselves to invoke it.
2. The language-selection rule — `entry.text.text(language)` then `.name` (and `.description` where modelled) — is stated in all four (with `None` hard-coded at the two organization-level sites because those operations model no `language`).
3. `lastUpdatedAt` -> `Some(crate::state::date_time(entry.last_updated_at_millis))`, `awsServices` -> `Some(entry.aws_services.clone())` and the always-absent `createdBy` / `updatedOnBehalfOf` / `updatedOnBehalfOfJobTitle` / `statusReason` decisions are likewise re-stated per site.

The model makes the duplication exact rather than coincidental: `RecommendationSummary`'s member set is a strict SUBSET of `Recommendation`'s (15 of 22 members, identical names and targets), and `OrganizationRecommendationSummary`'s is a strict subset of `OrganizationRecommendation`'s (14 of 21); and the two levels' entry types (`RecommendationEntry`, `OrganizationRecommendationEntry`) carry those classification members identically — which is exactly the premise `RecommendationFilterable` already exploits to state the listing predicate once (src/state.rs:935-976).

So four copies of one projection can drift: a member whose rule changes has to be changed in four places, and nothing in the crate forces the fourth. This is the same risk the crate already removed for the resource views, stated in its own words at src/state.rs:801-803.

The fix keeps every response byte-identical and every existing test unchanged, using the pattern the crate already has: state the shared projection once in `crate::state` (as `resource_summary` is), and have the full-shape handlers destructure it BY NAME and layer on the members only their shape models (as `organization_resource_summary` does at src/state.rs:838-868), reaching either entry type through a trait handing out the shared members (as `RecommendationFilterable` / `LifecycleWritable` already do).

## Evidence

The rule the crate states for a projection two surfaces report — services/service-com-amazonaws-trustedadvisor-trustedadvisor/src/state.rs:794-803:
```
/// The stored resource, as the modelled `RecommendationResourceSummary`.
///
/// This is the projection of a [`ResourceEntry`] that BOTH resource lists
/// report — `ListRecommendationResources` over one recommendation's resources
/// and `ListOrganizationRecommendationResources` over the member accounts'
/// resources an organization recommendation aggregates — so it is stated here
/// ONCE, as [`apply_lifecycle_update`] states the one lifecycle write of the two
/// levels. Reporting the two views from one projection is what makes them unable
/// to disagree about a resource, instead of agreeing only while two copies of
/// the projection happen to be written identically.
```
and the same reasoning applied to the listing filters — src/state.rs:939-944:
```
/// The two listings model the same eight filters over members the two entry
/// types carry identically, so they apply ONE rule to two kinds of entry. This
/// is the seam that lets that rule be stated once — by
/// [`RecommendationFilters`] — instead of once per operation, where the
/// account-level and the organization-level listing could drift apart on the
/// same members.
```

Copy 1 — src/ops/get_recommendation.rs:116-137 (clipped):
```
fn recommendation(
    entry: &crate::state::RecommendationEntry,
    resources_aggregates: RecommendationResourcesAggregates,
    pillar_specific_aggregates: Option<RecommendationPillarSpecificAggregates>,
    language: Option<&str>,
) -> Recommendation {
    let text = entry.text.text(language);
    Recommendation {
        id: entry.id.clone(),
        r#type: entry.r#type.clone(),
        check_arn: entry.check_arn.clone(),
        status: entry.status.clone(),
        lifecycle_stage: entry.lifecycle_stage.clone(),
        pillars: entry.pillars.clone(),
        source: entry.source.clone(),
        aws_services: Some(entry.aws_services.clone()),
        name: text.name.clone(),
        resources_aggregates,
        pillar_specific_aggregates,
        created_at: crate::state::priority_created_at(&entry.r#type, entry.created_at_millis),
        last_updated_at: Some(crate::state::date_time(entry.last_updated_at_millis)),
        arn: entry.arn.clone(),
```

Copy 2 — src/ops/list_recommendations.rs:145-167, the same lines over the same entry type:
```
fn recommendation_summary(
    entry: &crate::state::RecommendationEntry,
    resources_aggregates: RecommendationResourcesAggregates,
    pillar_specific_aggregates: Option<RecommendationPillarSpecificAggregates>,
    language: Option<&str>,
) -> RecommendationSummary {
    RecommendationSummary {
        id: entry.id.clone(),
        r#type: entry.r#type.clone(),
        check_arn: entry.check_arn.clone(),
        status: entry.status.clone(),
        lifecycle_stage: entry.lifecycle_stage.clone(),
        pillars: entry.pillars.clone(),
        source: entry.source.clone(),
        aws_services: Some(entry.aws_services.clone()),
        name: entry.text.text(language).name.clone(),
        resources_aggregates,
        pillar_specific_aggregates,
        created_at: crate::state::priority_created_at(&entry.r#type, entry.created_at_millis),
        last_updated_at: Some(crate::state::date_time(entry.last_updated_at_millis)),
        arn: entry.arn.clone(),
        status_reason: None,
    }
}
```

Copy 3 — src/ops/get_organization_recommendation.rs:98-121:
```
fn organization_recommendation(
    entry: &crate::state::OrganizationRecommendationEntry,
    resources_aggregates: RecommendationResourcesAggregates,
    pillar_specific_aggregates: Option<RecommendationPillarSpecificAggregates>,
) -> OrganizationRecommendation {
    // This operation models no `language`, so the catalog's default copy is the
    // one to report.
    let text = entry.text.text(None);
    OrganizationRecommendation {
        id: entry.id.clone(),
        ...
        created_at: crate::state::priority_created_at(&entry.r#type, entry.created_at_millis),
        last_updated_at: Some(crate::state::date_time(entry.last_updated_at_millis)),
        arn: entry.arn.clone(),
        description: text.description.clone(),
```

Copy 4 — src/ops/list_organization_recommendations.rs:129-152:
```
fn organization_recommendation_summary(
    entry: &crate::state::OrganizationRecommendationEntry,
    resources_aggregates: RecommendationResourcesAggregates,
    pillar_specific_aggregates: Option<RecommendationPillarSpecificAggregates>,
) -> OrganizationRecommendationSummary {
    // This operation models no `language`, so the catalog's default copy is the
    // one to report.
    let text = entry.text.text(None);
    OrganizationRecommendationSummary {
        id: entry.id.clone(),
        ...
        created_at: crate::state::priority_created_at(&entry.r#type, entry.created_at_millis),
        last_updated_at: Some(crate::state::date_time(entry.last_updated_at_millis)),
        arn: entry.arn.clone(),
    }
}
```

The model, which makes each summary a strict SUBSET of the full shape so one projection can serve both — `smithy_query(query="shape", shape="Recommendation,RecommendationSummary")`: every member of `RecommendationSummary` (`id, type, checkArn, status, lifecycleStage, pillars, source, awsServices, name, resourcesAggregates, pillarSpecificAggregates, createdAt, lastUpdatedAt, arn, statusReason`) is declared on `Recommendation` with the same name and target, and likewise every member of `OrganizationRecommendationSummary` on `OrganizationRecommendation`.

The pattern the fix would follow, already in the crate — src/state.rs:838-868, where the organization resource view destructures the shared projection BY NAME and replaces only the members its shape models differently:
```
    // Destructured by NAME, so every shared member reaches the organization
    // view as the projection computed it: the account-level
    // `recommendationArn` is the only one this view replaces outright.
    let RecommendationResourceSummary {
        id,
        arn,
        ...
    } = resource_summary(resource);
```

Existing tests that cover these four projections and must stay unchanged:
* tests/all/priority_scoped_created_at.rs — its header at :3-17 states it covers "the ONE rule the whole recommendation surface obeys on `createdAt` ... on the single read and on the listing alike" and "the organization surface goes on reporting a creation instant for all of them", i.e. all four sites;
* tests/all/localized_text.rs:20-25 — "`CheckSummary$name`/`$description`, `Recommendation$name`/`$description` and `RecommendationSummary$name` are prose", over every `RecommendationLanguage` value;
* tests/all/recommendation_loop.rs and tests/all/account_scoped_recommendation_list.rs (account-level read and listing), tests/all/organization_recommendation_read.rs and tests/all/organization_recommendations.rs (organization-level read and listing), tests/all/cost_optimizing_aggregates.rs (`pillarSpecificAggregates` on all four).

## Steps to Reproduce

1. Read src/state.rs:794-803: the crate states that a projection BOTH surfaces report is "stated here ONCE" so the two "cannot disagree ... instead of agreeing only while two copies of the projection happen to be written identically", and src/state.rs:939-944, which states the same for the listing filters.
2. Confirm the crate follows that rule for every other shared piece: `resource_summary` / `organization_resource_summary` (src/state.rs:804-868), `apply_lifecycle_update` + `LifecycleWritable` (src/state.rs:653-763), `RecommendationFilters` + `RecommendationFilterable` (src/state.rs:945-1064), `ResourceFilters` (src/state.rs:881-911) — all in `crate::state`, reached from both levels.
3. Now grep the recommendation projection: `rg -n 'priority_created_at\(&entry' services/service-com-amazonaws-trustedadvisor-trustedadvisor/src/ops` returns FOUR sites — get_recommendation.rs:135, list_recommendations.rs:163, get_organization_recommendation.rs:118, list_organization_recommendations.rs:149 — one per handler, none in `crate::state`.
4. Read the four functions in full (get_recommendation.rs:116-147, list_recommendations.rs:145-168, get_organization_recommendation.rs:98-129, list_organization_recommendations.rs:129-153): each re-states the same 14-15 member projection of a recommendation entry, including the Priority `createdAt` scoping, the `entry.text.text(language)` language selection, `Some(date_time(last_updated_at_millis))` and `Some(aws_services.clone())`.
5. Confirm from the model that the duplication is exact rather than coincidental: `smithy_query(query="shape", shape="Recommendation,RecommendationSummary,OrganizationRecommendation,OrganizationRecommendationSummary")` — each summary's member set is a strict subset of the corresponding full shape's, same names and targets.
6. Confirm the fix is response-preserving: state the shared projection once in `crate::state` and destructure it by name at the two full-shape sites, exactly as src/state.rs:842-855 already does for the resource views; the tests named in the evidence then still pass byte-for-byte.
