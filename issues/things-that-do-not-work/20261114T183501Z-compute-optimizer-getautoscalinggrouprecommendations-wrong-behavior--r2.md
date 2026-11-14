Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93

# A stored preferredResources include/exclude list never narrows the Auto Scaling group recommendation options

_Filed: 2026-11-14T18:35:01Z_
_Operation: GetAutoScalingGroupRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`preferredResources` is documented as available for exactly two resource types — the Amazon EC2 instance and the Auto Scaling group — and this surface is the second one. The handler builds the group's `recommendationOptions` from the stored candidate configurations filtered ONLY by the request's `cpuVendorArchitectures`; the effective preferences value is in scope at the same call site (read two lines below for `after_discounts()`/`infers_workload_types()`) and its `preferred_resources` lists are never consulted.

So a group-scoped (or account-scoped) exclude list is accepted by `PutRecommendationPreferences`, reported back resolved by `GetEffectiveRecommendationPreferences` and echoed in the recommendation's own `effectiveRecommendationPreferences.preferredResources`, while the recommendation still offers a configuration of an excluded instance type. The crate already holds the resolution (`crate::state::effective_include_list`, `crate::state::value_names_instance_type`); it is wired into the reporting path only.

## Evidence

Smithy, `PreferredResource` structure doc: "The preference to control which resource type values are considered when generating rightsizing recommendations." … note: "This preference is only available for the Amazon EC2 instance and Auto Scaling group resource types."
Smithy, `PreferredResource$excludeList`: "The preferred resource type values to exclude from the recommendation candidates."

Handler — the only filter applied to the candidates, src/ops/get_auto_scaling_group_recommendations.rs:471-477:
```
    let options: Vec<&crate::state::GroupConfiguration> = group
        .candidates
        .iter()
        .filter(|candidate| {
            crate::state::architecture_admitted(requested_architectures, &candidate.architecture)
        })
        .collect();
```
(`preferences: &crate::state::EffectivePreferences` is a parameter of this same `recommendation()` fn, src/ops/get_auto_scaling_group_recommendations.rs:462, used at :487 `preferences.after_discounts()` and :517 `preferences.infers_workload_types()`.)

The crate's own statement of the rule it does not apply, src/state/preferred_resources.rs:3-4: "How a preferred-resource preference's include and exclude lists RESOLVE into the effective set of rightsizing-recommendation candidates."

Test coverage: tests/all/auto_scaling_group_recommendations.rs:381-386 asserts only that the recommendation REPORTS `preferredResources`; tests/all/auto_scaling_group_recommendations.rs:399-409 (`the_cpu_vendor_architecture_preference_selects_the_options_reported`) is the only test that ties a preference to the options reported, and it covers `cpuVendorArchitectures` only — nothing pins the current behaviour for preferredResources.

## Steps to Reproduce

1. Boot the emulator, call UpdateEnrollmentStatus {"status":"Active"} and wait out crate::state::ANALYSIS_DELAY.
2. Call GetAutoScalingGroupRecommendations {} and note a group's autoScalingGroupArn plus an instance type X in recommendationOptions[].configuration.instanceType.
3. Call PutRecommendationPreferences {"resourceType":"AutoScalingGroup","scope":{"name":"ResourceArn","value":"<autoScalingGroupArn>"},"preferredResources":[{"name":"Ec2InstanceTypes","excludeList":["X"]}]} — answered 200 and stored.
4. Call GetEffectiveRecommendationPreferences {"resourceArn":"<autoScalingGroupArn>"} — X is absent from preferredResources[0].effectiveIncludeList.
5. Call GetAutoScalingGroupRecommendations {} again — recommendationOptions still offers configuration.instanceType X.
