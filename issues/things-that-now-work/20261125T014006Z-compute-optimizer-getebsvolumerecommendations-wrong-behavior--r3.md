Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 7a03c6c633814b42753ce8c8f03d4e7bdff82ea0

# A Finding filter carrying no values empties the volume answer, unlike every other filter path in the crate

_Filed: 2026-11-25T01:40:06Z_
_Operation: GetEBSVolumeRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `7a03c6c633814b42753ce8c8f03d4e7bdff82ea0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetEBSVolumeRecommendations` resolves each admitted `EBSFilter` into a handler-local `FindingFilter` whose `matches` requires the recommendation's finding to be one of the filter's values. For a filter that carries NO values (`{"name":"Finding","values":[]}`, or `values` omitted entirely — the member is optional in the model and `ebs_filter_rejection` refuses only a missing NAME or an out-of-vocabulary value), `self.findings` is empty, `iter().any(..)` is `false`, and EVERY volume is excluded: the reader answers an empty `volumeRecommendations` list.

Every other filter path in this same crate implements the opposite rule, and says so in its own doc comment: a filter carrying no values constrains nothing. That includes `crate::state::resource_filter_matches` (the EC2-instance and Auto Scaling group readers), `idle_recommendation_matches_filters`, the license and Lambda matchers, the ECS reader's own `RequestedFilters`, and — decisively — `row_matches_filters` in `state/export.rs`, which is the matcher the rows of `ExportEBSVolumeRecommendations` go through for the SAME `EBSFilters` list. So the identical filter list selects nothing through the reader and every volume row through the export, even though `state/export.rs` documents that matcher as behaving the way "the recommendation readers of this service answer for the same filter sets". One of the two must be wrong, and the EBS reader is the single outlier against eight sibling implementations of the rule.

## Evidence

Reader (the outlier) — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_ebs_volume_recommendations.rs:483-492:
```
impl FindingFilter {
    /// Whether a recommendation satisfies this filter: a filter is satisfied by ANY of
    /// its values, and a request's filters all have to be.
    fn matches(&self, recommendation: &VolumeRecommendation) -> bool {
        recommendation
            .finding
            .as_deref()
            .is_some_and(|finding| self.findings.iter().any(|wanted| wanted == finding))
    }
}
```
and :509-514 builds it straight from the request: `findings: filter.values.clone().unwrap_or_default(),` — so an absent/empty `values` yields an empty `findings`, and `.any(..)` is false for every volume. `ebs_filter_rejection` does not refuse that filter: src/state.rs:1217-1232 refuses only "a filter name must be supplied" and a value outside the vocabulary.

The crate's stated rule, broken by the code above — src/state.rs:2506 and :2528-2531:
```
/// A filter with no name, or one with no values, constrains nothing. `Finding` matches the
...
    let values = filter.values.as_deref().unwrap_or_default();
    if values.is_empty() {
        return true;
    }
```
The EXPORT of the very same recommendations, through the very same `EBSFilters` — src/state/export.rs:802-805 and :811-814:
```
/// `RecommendationSourceType` selects on
/// the resource type the job exports, which is the same for every row. Several filters are
/// ANDed and a filter carrying no values constrains nothing. A filter naming a member the
/// surface's own recommendation does not declare selects nothing, which is what the
/// recommendation readers of this service answer for the same filter sets.
...
    filters.iter().all(|(name, values)| {
        if values.is_empty() {
            return true;
        }
```
More siblings stating the same rule: src/state/idle.rs:574 "a filter carrying no values constrains nothing" (code at :582-584 `return true`); src/state/licenses.rs:495; src/state/functions.rs:724; src/ops/get_ecs_service_recommendations.rs:529-530 "/// The `Finding` values the request admits, or empty for every finding." with :538 `let finding = self.findings.is_empty() || ...`.

Test coverage of the two sides: tests/all/ebs_volume_recommendations.rs:482-483 pins the reader's outlier answer (`// A filter carrying no value selects nothing: no finding is one of none.` / `(json!([{"name": "Finding", "values": []}]), &[])`), while tests/all/ebs_volume_recommendations_export.rs:607-611 asserts only that the export ADMITS the same filter (its comment "a filter carrying no value, which selects nothing" is contradicted by export.rs:812, which keeps every row). tests/all/ec2_instance_recommendations.rs:796-797 covers "a filter carrying no values" on the sibling reader.

## Steps to Reproduce

1. Opt the account in (`UpdateEnrollmentStatus` with status `Active`) and let the analysis become due, with analysed EBS volumes in the Region (as `tests/all/ebs_volume_recommendations.rs` sets up).
2. Call `GetEBSVolumeRecommendations` with `{"filters":[{"name":"Finding","values":[]}]}` — the answer carries an EMPTY `volumeRecommendations` list (src/ops/get_ebs_volume_recommendations.rs:486-491).
3. Call `ExportEBSVolumeRecommendations` with the same `filters` and a destination bucket; once the job completes, the export file holds a row for EVERY analysed volume (src/state/export.rs:811-814), contradicting export.rs:803-805's claim that the matcher behaves as the reader does.
4. Call `GetEC2InstanceRecommendations` with `{"filters":[{"name":"Finding","values":[]}]}` — every analysed instance is reported, because src/state.rs:2529-2531 treats the valueless filter as no constraint.
