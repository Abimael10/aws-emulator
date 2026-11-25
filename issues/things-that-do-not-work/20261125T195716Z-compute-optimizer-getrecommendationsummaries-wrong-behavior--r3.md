Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 24511e4ce5aef4d33e503a00bb0e57c4ce26e21d

# Ec2Instance and AutoScalingGroup summaries price the recommendation from the UNFILTERED top candidate, ignoring the stored preferred-resources/utilization preference every other reader of the same analysis honours

_Filed: 2026-11-25T19:57:16Z_
_Operation: GetRecommendationSummaries | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `24511e4ce5aef4d33e503a00bb0e57c4ce26e21d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The summary reader groups the findings catalog by resource type and substitutes the RDS, EBS, Lambda and ECS groups with the analysis their own readers report from, precisely so the summary and the per-resource reader cannot disagree "about any one's finding, risk or cost". The `Ec2Instance` and `AutoScalingGroup` groups keep the catalog rows, and those rows are built by `ObservedInstance::to_analysed_resource` / `AnalyzedAutoScalingGroup::to_analysed_resource`, which price the recommendation from `candidates.first()` — the top candidate BEFORE any preference is applied — and are computed once when the store is constructed, so no preference stored later can ever move them.

Every other reader of the same analysis (the recommendation reader, the projection of its metrics, the export file) builds its options with `admitted_options(preferences, requested_architectures)`, which drops a candidate the account's stored `preferredResources` lists exclude and, for an instance, a candidate that no longer carries the workload under the stored `utilizationPreferences` headroom. So once such a preference is stored:

* the summary's `savingsOpportunity` / `aggregatedSavingsOpportunity` for the group is the saving of an instance type the preference excludes from every other answer; and where the preference leaves an instance NO admitted candidate, the reader reports an empty `recommendationOptions` (no saving at all) while the summary keeps claiming the excluded candidate's saving;
* `inferredWorkloadSavings` is grouped by `resource.inferred_workload_types` straight off the catalog row, so the savings are still reported under workload types for an account that deactivated the inference with the `inferredWorkloadTypes` preference — the member the instance reader then omits.

This is the same disagreement the RDS/EBS/Lambda/ECS substitutions in the very same handler exist to prevent.

## Evidence

The substitution the sibling groups get, and the group the instance/group summary keeps — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/ops/get_recommendation_summaries.rs:352-376:
```
        let mut analysed = guard.analysed_resources_by_type(&account_id);
        // The database rows are the analysed databases above ...
        analysed.insert(crate::state::SOURCE_TYPE_RDS_DB_INSTANCE, databases.iter().collect());
        ... analysed.insert(crate::state::SOURCE_TYPE_EBS_VOLUME, volumes.iter().collect());
        ... analysed.insert(crate::state::SOURCE_TYPE_LAMBDA_FUNCTION, functions.iter().collect());
        ... analysed.insert(crate::state::SOURCE_TYPE_ECS_SERVICE, ecs_services.iter().collect());
```
Nothing equivalent for `Ec2Instance`/`AutoScalingGroup`; the group used is the catalog's (src/ops/get_recommendation_summaries.rs:394-397 `let resources = analysed.get(resource_type)`), summed at src/ops/get_recommendation_summaries.rs:449-452 `.map(|resource| resource.monthly_savings())`.

The catalog row prices the UNFILTERED top candidate — src/state/instances.rs:583-586:
```
            recommended_monthly_cost: self
                .recommended_configuration()
                .unwrap_or(&self.current)
                .monthly_cost(),
```
with src/state/instances.rs:504-506:
```
    pub fn recommended_configuration(&self) -> Option<&InstanceConfiguration> {
        self.candidates.first()
    }
```
and the same for groups — src/state/auto_scaling_groups.rs:220-224:
```
            recommended_monthly_cost: self
                .candidates
                .first()
                .unwrap_or(&self.current)
                .monthly_cost(),
```

The claim the code under it breaks — src/state/instances.rs:566-570:
```
    /// The projection is what keeps one instance's analysis in ONE place: the summary reader
    /// and every reader of an instance recommendation answer from this observation, so they
    /// cannot disagree about an instance's finding, its performance risk or what adopting its
    /// recommendation saves.
```
and src/state/auto_scaling_groups.rs:201-204:
```
    /// The projection is what keeps one group's analysis in ONE place: the summary reader
    /// and `GetAutoScalingGroupRecommendations` both answer from this row's source, so
    /// they cannot disagree about a group's finding, its performance risk or what adopting
    /// its recommendation saves.
```
and src/state/instances.rs:74-76:
```
    /// instance type the preference leaves out of the candidates is an option of no answer,
    /// and no reader contradicts the candidate set a read of that preference resolves.
```

What the per-resource reader actually prices — src/ops/get_ec2_instance_recommendations.rs:524 `let options = instance.admitted_options(preferences, requested_architectures);` and :538 `let (saved, percentage) = instance.savings(option);`; src/ops/get_auto_scaling_group_recommendations.rs:501 `let options = group.admitted_options(preferences, requested_architectures);`. The preference filter itself is src/state/instances.rs:300-310 (`option_admitted` + `carries_analysed_utilization`).

The inferred-workload half — src/ops/get_recommendation_summaries.rs:602-608 groups by `resource.inferred_workload_types` unconditionally, while src/ops/get_ec2_instance_recommendations.rs:607-609 reports the member only `preferences.infers_workload_types().then(...)`.

Test coverage: tests/all/preferred_resource_recommendation_candidates.rs:256-259 states the rule and covers exactly three readers — "The three readers of ONE instance analysis report the same narrowed options: the recommendation reader, the projection of an option's metrics and the export file. A preference that is honoured by one and not another would let a client see an instance type recommended by one path and excluded by the next." `GetRecommendationSummaries` is not among them, and tests/all/recommendation_summaries.rs / tests/all/tracked_resource_type_summaries.rs assert the summary surface with no preference stored.

## Steps to Reproduce

1. Start the emulator, call UpdateEnrollmentStatus with status=Active and let ANALYSIS_DELAY elapse so the analysis is visible.
2. Call GetRecommendationSummaries and read the summary whose recommendationResourceType is Ec2Instance: its savingsOpportunity.estimatedMonthlySavings is computed from m5.xlarge (the seeded top candidate of i-0a1b2c3d4e5f60001, $0.192/h → 140.16/month against the m5.2xlarge current cost of 280.32).
3. Call PutRecommendationPreferences with {resourceType: "Ec2Instance", scope: {name: "AccountId", value: <account>}, preferredResources: [{name: "Ec2InstanceTypes", excludeList: ["m5"]}]}.
4. Call GetEC2InstanceRecommendations: the rank-1 option of i-0a1b2c3d4e5f60001 is now m6g.xlarge ($0.154/h) with estimatedMonthlySavings 167.90 — m5.xlarge is excluded from the options, the projection and the export file.
5. Call GetRecommendationSummaries again: the Ec2Instance savingsOpportunity is still the 140.16 of the excluded m5.xlarge candidate.
6. Repeat with an exclude list that removes every candidate (e.g. excludeList ["m5", "m6g"]): GetEC2InstanceRecommendations reports an empty recommendationOptions list for that instance, while GetRecommendationSummaries still reports the 140.16 saving.
7. Store {resourceType: "Ec2Instance", scope: {...}, inferredWorkloadTypes: "Inactive"} and compare: the instance recommendation omits inferredWorkloadTypes, while the summary still reports inferredWorkloadSavings keyed by ["PostgreSql"]/["Redis"].
