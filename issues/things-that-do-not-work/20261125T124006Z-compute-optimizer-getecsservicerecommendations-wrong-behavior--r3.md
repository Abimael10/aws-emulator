Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 4cc6d9f20fda2c3a5762540e0b64859285d72782

# GetECSServiceRecommendations merges repeated filters of one name into a single OR set, while every sibling reader ANDs each filter entry

_Filed: 2026-11-25T12:40:06Z_
_Operation: GetECSServiceRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `4cc6d9f20fda2c3a5762540e0b64859285d72782`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other filtered reader of this crate applies the request's `filters` entry by entry and requires ALL of them to be satisfied (`filters.iter().all(...)`), a rule stated verbatim in four shared matchers as "Several filters are ANDed". `GetECSServiceRecommendations` instead flattens the list into two per-NAME value buckets before matching: `requested_filters` does `values.extend(filter.values.iter().flatten().cloned())` into one `Vec` per filter name, so two `Finding` entries in one request are ORed rather than ANDed.

Consequence: the request body `{"filters":[{"name":"Finding","values":["Optimized"]},{"name":"Finding","values":["Underprovisioned"]}]}` selects NOTHING on GetEBSVolumeRecommendations / GetLambdaFunctionRecommendations / GetLicenseRecommendations / GetRDSDatabaseRecommendations / GetIdleRecommendations / GetEC2InstanceRecommendations (two mutually exclusive conditions ANDed), but on GetECSServiceRecommendations it returns the union of both findings. One crate, one `filters` concept, two incompatible conjunction semantics — and the ECS reader is also the one surface whose reader result is asserted to equal the export's result for the same filters (`tests/all/ecs_service_recommendations.rs:2836`), while the shared export filter path is a different implementation again.

## Evidence

The ECS reader flattens per name — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_ecs_service_recommendations.rs:570-589:
```
    let mut requested = RequestedFilters::default();
    for filter in filters {
        let values = match filter.name.as_deref() {
            Some(crate::state::ECS_FILTER_FINDING) => &mut requested.findings,
            Some(crate::state::ECS_FILTER_FINDING_REASON_CODE) => {
                &mut requested.finding_reason_codes
            }
...
        values.extend(filter.values.iter().flatten().cloned());
```
and matches only the two merged buckets — get_ecs_service_recommendations.rs:537-550:
```
    fn matches(&self, recommendation: &ECSServiceRecommendation) -> bool {
        let finding = self.findings.is_empty()
            || recommendation
                .finding
                .as_ref()
                .is_some_and(|finding| self.findings.contains(finding));
```

The rule every sibling states and implements — src/state/licenses.rs:493-502:
```
/// A filter matches when the member its name selects on equals any of the filter's values —
/// for the reason codes, which are a LIST, when any reported code is among them. Several
/// filters are ANDed, and a filter carrying no values constrains nothing.
...
    filters.iter().all(|filter| {
```
src/state/functions.rs:722-731 (same sentence, same `filters.iter().all(|filter| {`), src/state.rs:2622-2631 (`rds_recommendation_matches_filters`, same sentence, same `.all`), src/state/idle.rs:573-581 ("several filters are ANDed" + `filters.iter().all(|filter| {`).

The same per-entry conjunction in the handlers — src/ops/get_ebs_volume_recommendations.rs:484-486:
```
    /// Whether a recommendation satisfies this filter: a filter is satisfied by ANY of
    /// its values, and a request's filters all have to be.
```
applied at get_ebs_volume_recommendations.rs:330:
```
            .filter(|recommendation| filters.iter().all(|filter| filter.matches(recommendation)))
```
and src/ops/get_ec2_instance_recommendations.rs:319-325:
```
            .filter(|(instance, _, preferences)| {
                input
                    .filters
                    .iter()
                    .flatten()
                    .all(|filter| filter_matches(filter, instance, preferences))
            })
```

Test coverage: the ECS filter tests only ever send ONE entry per name — tests/all/ecs_service_recommendations.rs:1203-1221 (`Finding=Overprovisioned`, `FindingReasonCode=CPUUnderprovisioned`, `Finding=Optimized matches nothing here`) and tests/all/ecs_service_recommendations.rs:2838-2866, whose only multi-entry row uses two DIFFERENT names ("a finding and a reason code no service carries together"). No test pins the repeated-name case, so nothing justifies the divergence.

## Steps to Reproduce

1. Boot the emulator with ECS, CloudWatch and Compute Optimizer mounted; call UpdateEnrollmentStatus(status=Active) and wait out crate::state::ANALYSIS_DELAY.
2. Create two ACTIVE Fargate ECS services whose analysis reaches different findings (as tests/all/ecs_service_recommendations.rs does: one Overprovisioned, one Underprovisioned).
3. Call GetECSServiceRecommendations with {"filters":[{"name":"Finding","values":["Overprovisioned"]},{"name":"Finding","values":["Underprovisioned"]}]}.
4. Observe HTTP 200 with BOTH recommendations: the two entries were unioned.
5. Send the analogous body to GetEBSVolumeRecommendations or GetLambdaFunctionRecommendations ({"filters":[{"name":"Finding","values":["Optimized"]},{"name":"Finding","values":["NotOptimized"]}]}) and observe an EMPTY list: those readers AND the two entries, as state/licenses.rs:495 and get_ebs_volume_recommendations.rs:485 both state.
