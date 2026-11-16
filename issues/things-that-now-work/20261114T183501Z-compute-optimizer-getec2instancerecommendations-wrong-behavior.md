Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93

# A stored preferredResources include/exclude list never narrows the reported recommendation options

_Filed: 2026-11-14T18:35:01Z_
_Operation: GetEC2InstanceRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1f5d5a2e3955ce51e23f36cd9ec02202b9df7d93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`PutRecommendationPreferences$preferredResources` is stored, validated, and reported back as effective (both by `GetEffectiveRecommendationPreferences` and in each recommendation's `effectiveRecommendationPreferences.preferredResources`), but it has NO effect on the instance types `GetEC2InstanceRecommendations` actually recommends.

The handler builds `recommendationOptions` from the stored candidate list filtered ONLY by the request's `cpuVendorArchitectures`; the effective `preferred_resources` lists are in scope at that very call site (the same `preferences` value is read two lines later for `after_discounts()`) and are never consulted. So an account that excludes an instance type is still recommended that instance type, while `GetEffectiveRecommendationPreferences` simultaneously reports it as excluded from `effectiveIncludeList` — a self-contradicting pair of answers about the same resource.

The crate already holds the whole resolution for this (`crate::state::effective_include_list`, `crate::state::value_names_instance_type`); it is wired only into the REPORTING path, not into candidate selection.

Secondary symptom of the same class on this operation: `utilizationPreferences` ("The preference to control the resource's CPU utilization threshold, CPU utilization headroom, and memory utilization headroom") is likewise stored and reported but never reaches any sizing decision — the only headroom the crate applies is the fixed `pub const RECOMMENDATION_HEADROOM: f64 = 0.20;` (src/state.rs:1247), which is never derived from the stored preference.

The same omission is present in the export of these recommendations (src/state/export.rs:983) and in `GetEC2RecommendationProjectedMetrics` (src/ops/get_ec2_recommendation_projected_metrics.rs:325-328), so no path of the EC2 surface honours the preference.

## Evidence

Smithy, `PreferredResource$excludeList` (smithy_query doc): "The preferred resource type values to exclude from the recommendation candidates. If this isn't specified, all supported resources are included by default."

Smithy, `PreferredResource` structure doc: "The preference to control which resource type values are considered when generating rightsizing recommendations. You can specify this preference as a combination of include and exclude lists." … "This preference is only available for the Amazon EC2 instance and Auto Scaling group resource types."

Handler — the only filter applied to the candidates, src/ops/get_ec2_instance_recommendations.rs:506-512:
```
    let options: Vec<&crate::state::InstanceConfiguration> = instance
        .candidates
        .iter()
        .filter(|candidate| {
            crate::state::architecture_admitted(requested_architectures, &candidate.architecture)
        })
        .collect();
```
(`preferences: &crate::state::EffectivePreferences` is the third parameter of this same `recommendation()` fn, src/ops/get_ec2_instance_recommendations.rs:496, and is used at :521 `let after_discounts = preferences.after_discounts();` — the preferred-resource lists on it are never read.)

The crate's own statement of the rule it does not apply, src/state/preferred_resources.rs:3-4:
```
//! How a preferred-resource preference's include and exclude lists RESOLVE into the
//! effective set of rightsizing-recommendation candidates.
```
and src/state/preferred_resources.rs:136-138:
```
/// instance type the exclude list names is then left out, because those are the values "to
/// exclude from the recommendation candidates": a candidate set that still held them would
/// report an exclusion as having had no effect.
```

The field that holds the lists at the recommendation site, src/state.rs:1717-1722:
```
    /// `PreferredResources` as the preference STATES them, which default to
    /// [`default_preferred_resources`]. Held as the written lists rather than as candidate
    /// sets, so the resolution a reader reports ([`effective_preferred_resource`]) is run
    /// where that reader builds its response — for the items of one page — and not for every
    /// resource a request selected on the way there.
    pub preferred_resources: Vec<PreferredResource>,
```

Test coverage: tests/all/effective_preferred_resource_candidates.rs asserts only that a written list is READ BACK resolved (`effectiveIncludeList`); no test asserts anything about the recommendation options under such a preference, so nothing pins the current behaviour. tests/all/ec2_instance_recommendations.rs covers the options under `cpuVendorArchitectures` only.

## Steps to Reproduce

1. Boot the emulator, call UpdateEnrollmentStatus {"status":"Active"} and wait out crate::state::ANALYSIS_DELAY so the analysis is due.
2. Call GetEC2InstanceRecommendations {} and note one recommendation's instanceArn plus an instance type X appearing in its recommendationOptions[].instanceType.
3. Call PutRecommendationPreferences {"resourceType":"Ec2Instance","scope":{"name":"AccountId","value":"<account>"},"preferredResources":[{"name":"Ec2InstanceTypes","excludeList":["X"]}]} — answered 200 and stored.
4. Call GetEffectiveRecommendationPreferences {"resourceArn":"<instanceArn>"} — X is absent from preferredResources[0].effectiveIncludeList, i.e. the service reports X as excluded from the candidates.
5. Call GetEC2InstanceRecommendations {} again — recommendationOptions still carries instanceType X (and effectiveRecommendationPreferences.preferredResources on the same recommendation says X is excluded).
