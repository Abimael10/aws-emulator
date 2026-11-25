Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: c6dd973eb6cd65a31ae8eee55c5b01fc505934bd

# The "member documented for one target resource type only" rule skips CreateRule.Scripts, so a VOLUME-targeting policy stores and reports an instance-only member

_Filed: 2026-11-24T21:09:20Z_
_Operation: UpdateLifecyclePolicy | Class: validation_

> _Note: this issue was identified against the code base at commit `c6dd973eb6cd65a31ae8eee55c5b01fc505934bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/state/target_resource_type.rs` is the crate's one implementation of the rule "a configuration member whose documentation admits ONE target resource type is refused for a policy that targets the other". Both write paths run it (`update_lifecycle_policy.rs:335-341`, `create_lifecycle_policy.rs:389-395`).

`CreateRule$Scripts` carries exactly the same documentation bracket as two members the rule already judges — `Parameters$ExcludeBootVolume` and `Parameters$ExcludeDataVolumeTags`, both "[Custom snapshot policies that target instances only]" and both listed with `admitted_target: INSTANCE_TARGET` — yet `Scripts` appears neither in the module's own list of restricted members (its header doc enumerates four) nor in `TARGET_RESTRICTED_MEMBERS`.

The consequence is the very thing the module says it exists to prevent: a custom snapshot policy whose `PolicyDetails.ResourceTypes` is `["VOLUME"]` can be created, and can be mutated by `UpdateLifecyclePolicy` into, a configuration whose schedule's `CreateRule.Scripts` is set — a member its target admits nowhere — and every later `GetLifecyclePolicy` reports it. The sibling member `Parameters.ExcludeBootVolume`, documented with the identical sentence, is refused 400 `InvalidRequestException` on the same request.

(The open issue `20261124T080355Z-dlm-createlifecyclepolicy-validation--r8` covers a different omission of the same list — the three `Exclusions` members on the DEFAULT-policy side, which the helper never reaches because it reads `details.resource_types`. This one is on the CUSTOM-policy side, in the branch that does run.)

## Evidence

Smithy — `smithy_query(query="doc", shape="CreateRule", member="Scripts")`:
"<b>[Custom snapshot policies that target instances only]</b> Specifies pre and/or post scripts for a snapshot lifecycle policy that targets instances."

The identical bracket on a member the rule DOES judge — `smithy_query(query="shape", shape="Parameters")`, member `ExcludeBootVolume`:
"<b>[Custom snapshot policies that target instances only]</b> Indicates whether to exclude the root volume from multi-volume snapshot sets."

The crate's own rule, services/service-com-amazonaws-dlm-dlm-20180112/src/state/target_resource_type.rs:13-18:
"Several configuration members are documented for policies that target ONE of those two resource types only, so a request stating one for a policy that targets the other states a configuration the service cannot apply — the \"invalid parameters\" the write operations model `InvalidRequestException` for. It is refused before anything is stored, so no stored policy carries a member the resource type it targets admits nowhere and no read reports one."

The member list it is applied to — target_resource_type.rs:91-112 (four entries, `Scripts` absent):
```
const TARGET_RESTRICTED_MEMBERS: [TargetRestrictedMember; 4] = [
    TargetRestrictedMember { name: "VariableTags", admitted_target: INSTANCE_TARGET, stated: variable_tags_stated },
    TargetRestrictedMember { name: "ArchiveRule", admitted_target: VOLUME_TARGET, stated: archive_rule_stated },
    TargetRestrictedMember { name: "ExcludeBootVolume", admitted_target: INSTANCE_TARGET, stated: exclude_boot_volume_stated },
    TargetRestrictedMember { name: "ExcludeDataVolumeTags", admitted_target: INSTANCE_TARGET, stated: exclude_data_volume_tags_stated },
];
```
and the module's own enumeration of the restricted members, target_resource_type.rs:24-35, which lists `Schedule$VariableTags`, `Schedule$ArchiveRule`, `Parameters$ExcludeBootVolume` and `Parameters$ExcludeDataVolumeTags` and stops there — while the schedule walk it uses (`fn schedules`, target_resource_type.rs:140-142) already reaches every `Schedule`, and so every `CreateRule`, it would need.

The member exists on the deserialized type, so the value reaches the store — services/service-com-amazonaws-dlm-dlm-20180112/src/ops/types/shared_types.rs:74-81:
```
pub struct CreateRule {
    pub location: Option<String>,
    pub interval: Option<i32>,
    pub interval_unit: Option<String>,
    pub times: Option<Vec<String>>,
    pub cron_expression: Option<String>,
    pub scripts: Option<Vec<Script>>,
}
```

The update path that stores it unjudged — services/service-com-amazonaws-dlm-dlm-20180112/src/ops/update_lifecycle_policy.rs:335-341:
```
        if let Some(refusal) =
            crate::state::target_resource_type_violation(input.policy_details.as_ref())
        {
```
followed by update_lifecycle_policy.rs:423-427, which materializes and stores the replacement configuration whole (`policy.policy_details = Some(details);`).

Test coverage: `grep -r Scripts services/service-com-amazonaws-dlm-dlm-20180112/tests` matches nothing — no test exercises or pins the member, and no comment in the crate states a reason for admitting an instance-only member on a VOLUME-targeting policy. The analogous refusals for the four listed members are covered by tests/all/policy_configuration_validation.rs (the `VOLUME` + `Parameters.ExcludeBootVolume` rows) and are unaffected.

## Steps to Reproduce

1. POST /policies with a valid `ExecutionRoleArn`, `Description`, `State: "ENABLED"` and `PolicyDetails: {"PolicyType": "EBS_SNAPSHOT_MANAGEMENT", "ResourceTypes": ["VOLUME"], "TargetTags": [{"Key": "backup", "Value": "true"}], "Schedules": [{"CreateRule": {"Interval": 24, "IntervalUnit": "HOURS"}, "RetainRule": {"Count": 3}}]}` — a custom snapshot policy that targets VOLUME. Note the PolicyId.
2. PATCH /policies/{PolicyId} with the same `PolicyDetails`, adding `"Scripts": [{"ExecutionHandler": "AWS_VSS_BACKUP", "Stages": ["PRE"]}]` inside the schedule's `CreateRule`.
3. Observe HTTP 200 instead of the 400 `InvalidRequestException` ("The following parameter(s) are invalid for Resource Type {VOLUME}: Scripts") the crate answers for a member the policy's target does not admit.
4. GET /policies/{PolicyId} and observe `/Policy/PolicyDetails/Schedules/0/CreateRule/Scripts` reported on a VOLUME-targeting policy.
5. Compare with the sibling member carrying the identical documentation bracket: PATCH the same policy with `"Parameters": {"ExcludeBootVolume": true}` and observe 400 `InvalidRequestException` naming `ExcludeBootVolume`.
6. Mirror the create path: POST /policies with the step-1 body plus `CreateRule.Scripts` and observe HTTP 200 and a minted PolicyId.
