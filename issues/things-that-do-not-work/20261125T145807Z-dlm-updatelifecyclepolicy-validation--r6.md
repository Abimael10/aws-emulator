Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: a11c67ddfc7199c8950e9df279eea2cbb1f1bbbc

# The "member documented for one kind of policy only" rule is implemented for the TARGET resource type but never for the POLICY TYPE, so a snapshot policy stores DeprecateRule/NoReboot and an AMI policy stores ShareRules

_Filed: 2026-11-25T14:58:07Z_
_Operation: UpdateLifecyclePolicy | Class: validation_

> _Note: this issue was identified against the code base at commit `a11c67ddfc7199c8950e9df279eea2cbb1f1bbbc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate implements the "a configuration member whose documentation names one kind of policy is refused for a policy of the other kind" rule twice already:

* by POLICY KIND — `crate::state::custom_policy_only_parameters_in_policy_details` / `DefaultPolicyOnlyRequest::violation` refuse a `[Default policies only]` member stated for a custom policy and a custom-only member stated for a default policy;
* by TARGET RESOURCE TYPE — `src/state/target_resource_type.rs` refuses `VariableTags`, `Scripts`, `ArchiveRule`, `ExcludeBootVolume` and `ExcludeDataVolumeTags` for a policy whose `PolicyDetails$ResourceTypes` is not the one those members' documentation admits.

The third dimension the same family of sentences uses — the POLICY TYPE (`PolicyDetails$PolicyType`: `EBS_SNAPSHOT_MANAGEMENT` vs `IMAGE_MANAGEMENT` vs `EVENT_BASED_POLICY`) — is judged by nothing. `target_resource_type.rs` says so in its own module doc, and no other helper takes it up: the only member-admissibility checks either write path runs are `DefaultPolicyOnlyRequest::violation`, `custom_policy_only_violation` and `target_resource_type_violation`.

So `UpdateLifecyclePolicy` accepts and stores, and every later read reports:

* `Schedule$DeprecateRule` (`[Custom AMI policies only]`) and `CrossRegionCopyRule$DeprecateRule` (`[Custom AMI policies only]`) on an `EBS_SNAPSHOT_MANAGEMENT` policy;
* `Parameters$NoReboot` (`[Custom AMI policies only]`) on an `EBS_SNAPSHOT_MANAGEMENT` policy;
* `Schedule$ShareRules` (`ShareRule` is `[Custom snapshot policies only]`) on an `IMAGE_MANAGEMENT` policy.

The same gap is on the create path, which calls the identical helper set (`src/ops/create_lifecycle_policy.rs:389-395`); it is filed here because the `UpdateLifecyclePolicy` handler is the one that overwrites an already-accepted policy's configuration with such a member, and because the previous finding against this same helper was filed and fixed on this operation (`20261124T210920Z-dlm-updatelifecyclepolicy-validation--r5`, "The 'member documented for one target resource type only' rule skips CreateRule.Scripts"). The crate's module doc for that rule states the consequence it exists to prevent — "It is refused before anything is stored, so no stored policy carries a member the resource type it targets admits nowhere and no read reports one" — and the policy-type half of exactly those sentences leaves records holding members the kind of policy they are admits nowhere.

## Evidence

Smithy — the sentences that name a POLICY TYPE, each the same bracketed form as the `[Default policies only]` and `[... that target instances only]` sentences this crate already enforces:

* `smithy_query(query="doc", shape="Schedule", member="DeprecateRule")`: "<b>[Custom AMI policies only]</b> The AMI deprecation rule for the schedule."
* `smithy_query(query="shape", shape="Parameters")`, member `NoReboot`: "<b>[Custom AMI policies only]</b> Indicates whether targeted instances are rebooted when the lifecycle policy runs."
* `smithy_query(query="shape", shape="ShareRule")`: "<b>[Custom snapshot policies only]</b> Specifies a rule for sharing snapshots across Amazon Web Services accounts."
* `smithy_query(query="shape", shape="CrossRegionCopyRule")`, member `DeprecateRule`: "<b>[Custom AMI policies only]</b> The AMI deprecation rule for cross-Region AMI copies created by the rule."
* `smithy_query(query="shape", shape="PolicyDetails")`, member `Schedules`: "<b>[Custom snapshot and AMI policies only]</b> The schedules of policy-defined actions for snapshot and AMI lifecycle policies."

The crate's own rule, services/service-com-amazonaws-dlm-dlm-20180112/src/state/target_resource_type.rs:13-18:
```
//! Several configuration members are documented for policies that target ONE of
//! those two resource types only, so a request stating one for a policy that
//! targets the other states a configuration the service cannot apply — the
//! "invalid parameters" the write operations model `InvalidRequestException`
//! for. It is refused before anything is stored, so no stored policy carries a
//! member the resource type it targets admits nowhere and no read reports one.
```
and the half it leaves out, target_resource_type.rs:48-51:
```
//! What those sentences say about the policy TYPE that takes a member is a
//! different member of the configuration (`PolicyDetails$PolicyType`) and is not
//! judged here: this module judges the target resource type alone, which is the
//! fact the refusal names.
```
The list the rule is applied to names only target-restricted members — target_resource_type.rs:102-128 `const TARGET_RESTRICTED_MEMBERS: [TargetRestrictedMember; 5]` (`VariableTags`, `Scripts`, `ArchiveRule`, `ExcludeBootVolume`, `ExcludeDataVolumeTags`) — and the helper reads only the target, target_resource_type.rs:192-199:
```
pub fn target_resource_type_violation(
    details: Option<&PolicyDetails>,
) -> Option<InvalidRequestException> {
    let details = details?;
    let targets = details.resource_types.as_deref()?;
```

The SIBLING rule that does refuse members by the kind of policy — src/state.rs:1796-1815:
```
pub fn custom_policy_only_parameters_in_policy_details(
    details: Option<&PolicyDetails>,
) -> Vec<String> {
    ...
    [
        (RESOURCE_TYPES_MEMBER, details.resource_types.is_some()),
        ("ResourceLocations", details.resource_locations.is_some()),
        ("TargetTags", details.target_tags.is_some()),
        (SCHEDULES_MEMBER, details.schedules.is_some()),
        ("Parameters", details.parameters.is_some()),
        ("EventSource", details.event_source.is_some()),
        ("Actions", details.actions.is_some()),
    ]
```
— i.e. the crate already treats a bracketed "only" sentence as a refusal rule; it just never does it for the policy-type bracket.

That the crate DOES read the policy-type bracket elsewhere, and only to decide what to materialize — src/state.rs:587-590:
```
    // `CreateRule$Location` is `[Custom snapshot policies only]`, so an AMI
    // policy's creation rule has no location to report. `PolicyDetails$PolicyType`
    // is what states the type, stamped just above.
    let creates_snapshots = details.policy_type.as_deref() != Some(IMAGE_POLICY_TYPE);
```

The update path runs no other admissibility check — src/ops/update_lifecycle_policy.rs:335-341:
```
        if let Some(refusal) =
            crate::state::target_resource_type_violation(input.policy_details.as_ref())
        {
            return Err(
                UpdateLifecyclePolicyErrorModeled::InvalidRequestException(refusal).into(),
            );
        }
```
and then stores the replacement wholesale, update_lifecycle_policy.rs:423-427:
```
            let mut details = crate::state::materialized_policy_details(policy_details, kind);
            ...
            policy.policy_details = Some(details);
```

Test coverage: no test asserts the violating combination succeeds. tests/all/schedule_rule_forms.rs:250-278 deliberately pairs each nested rule with the policy kind its documentation declares, naming the very brackets this issue is about:
```
/// The kind of policy whose documentation declares the rule under test:
/// `FastRestoreRule` and `ArchiveRule` are `[Custom snapshot policies only]`,
/// `DeprecateRule` is `[Custom AMI policies only]`
/// (`smithy_query(query="shape", shape="Schedule")`).
enum PolicyKind { Snapshot, Image }
```
so every `DeprecateRule` row uses `"PolicyType": "IMAGE_MANAGEMENT"` (schedule_rule_forms.rs:270-275) and every `ShareRules` row uses `"PolicyType": "EBS_SNAPSHOT_MANAGEMENT"` (tests/all/schedule_retention_floor.rs:256-257). `NoReboot` appears in no test at all. The analogous target-type refusals are pinned by tests/all/policy_configuration_validation.rs:1128-1205, which stay unchanged.

## Steps to Reproduce

1. POST /policies with a valid `ExecutionRoleArn`, `Description`, `State: "ENABLED"` and `PolicyDetails: {"PolicyType": "EBS_SNAPSHOT_MANAGEMENT", "ResourceTypes": ["VOLUME"], "TargetTags": [{"Key":"backup","Value":"daily"}], "Schedules": [{"Name":"daily","CreateRule":{"Interval":24,"IntervalUnit":"HOURS"},"RetainRule":{"Count":2}}]}` — observe HTTP 200 and note the PolicyId.
2. PATCH /policies/{PolicyId} with the same `PolicyDetails` but with `"DeprecateRule": {"Count": 1}` added to the schedule — a member documented `[Custom AMI policies only]` on an `EBS_SNAPSHOT_MANAGEMENT` policy. Observe HTTP 200 instead of the `InvalidRequestException` the crate answers for a member the policy's kind does not admit.
3. GET /policies/{PolicyId} and observe `/Policy/PolicyDetails/Schedules/0/DeprecateRule` reported on a snapshot policy.
4. Repeat with `"Parameters": {"NoReboot": true}` (documented `[Custom AMI policies only]`) on the same `EBS_SNAPSHOT_MANAGEMENT` policy — observe HTTP 200.
5. Mirror case: create an `IMAGE_MANAGEMENT` policy targeting `INSTANCE`, then PATCH it with a schedule carrying `"ShareRules": [{"TargetAccounts": ["111122223333"]}]` (`ShareRule` is documented `[Custom snapshot policies only]`) and `"RetainRule": {"Count": 1}` — observe HTTP 200.
6. Compare with the dimension that IS enforced: the same `IMAGE_MANAGEMENT` policy targeting `INSTANCE` stating a schedule `ArchiveRule` is refused 400 `InvalidRequestException` by `target_resource_type_violation`, because that member's bracket names a TARGET rather than a policy type.
7. Same sequence through POST /policies (the create path calls the identical helper set at src/ops/create_lifecycle_policy.rs:389-395), confirming the gap is in the shared rule rather than in one handler.
