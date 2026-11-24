Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 831ce6778f4fe61ae9858b780889b40df5a51fbd

# The "member documented for one resource type only" rule skips every Exclusions member, so a default policy stores exclusions its kind admits nowhere

_Filed: 2026-11-24T08:03:55Z_
_Operation: CreateLifecyclePolicy | Class: validation_

> _Note: this issue was identified against the code base at commit `831ce6778f4fe61ae9858b780889b40df5a51fbd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/state/target_resource_type.rs` implements one rule for this crate: a configuration member whose documentation admits ONE target resource type is refused for a policy that targets the other, "refused before anything is stored, so no stored policy carries a member the resource type it targets admits nowhere and no read reports one".

The rule is applied to exactly four members (`Schedule$VariableTags`, `Schedule$ArchiveRule`, `Parameters$ExcludeBootVolume`, `Parameters$ExcludeDataVolumeTags`), and the helper reads the target from `details.resource_types` only — the member a DEFAULT policy may not state at all (`custom_policy_only_parameters_in_policy_details` refuses `PolicyDetails.ResourceTypes` for one). So for a DEFAULT policy the helper always returns `None` on its first line and judges nothing.

But the three members of `Exclusions` carry exactly the same kind of restriction, stated for the two kinds of DEFAULT policy:

* `Exclusions$ExcludeBootVolumes` and `Exclusions$ExcludeVolumeTypes` are `[Default policies for EBS snapshots only]` — i.e. a default policy whose `ResourceType`/`DefaultPolicy` is `VOLUME`;
* `Exclusions$ExcludeTags` is `[Default policies for EBS-backed AMIs only]` — i.e. one whose `ResourceType`/`DefaultPolicy` is `INSTANCE`.

Neither write path judges them. `CreateLifecyclePolicy` takes `Exclusions` straight onto the record and `UpdateLifecyclePolicy` overwrites it, so an `INSTANCE` (EBS-backed AMI) default policy can be created and read back carrying `ExcludeBootVolumes`/`ExcludeVolumeTypes`, and a `VOLUME` (EBS snapshot) default policy carrying `ExcludeTags` — precisely the "stored policy carries a member the resource type it targets admits nowhere" the module says it exists to prevent. `TARGET_RESTRICTED_MEMBERS` names none of the three.

## Evidence

Smithy — `smithy_query(query="shape", shape="Exclusions")`:
* `ExcludeBootVolumes`: "<b>[Default policies for EBS snapshots only]</b> Indicates whether to exclude volumes that are attached to instances as the boot volume."
* `ExcludeVolumeTypes`: "<b>[Default policies for EBS snapshots only]</b> Specifies the volume types to exclude. Volumes of the specified types will not be targeted by the policy."
* `ExcludeTags`: "<b>[Default policies for EBS-backed AMIs only]</b> Specifies whether to exclude volumes that have specific tags."

Which kind of default policy each bracket names — `smithy_query(query="shape", shape="PolicyDetails")`, member `ResourceType` (same text as `CreateLifecyclePolicyRequest$DefaultPolicy`): "To create a default policy for EBS snapshots, that creates snapshots of all volumes in the Region that do not have recent backups, specify <code>VOLUME</code>. ... To create a default policy for EBS-backed AMIs ... specify <code>INSTANCE</code>."

The crate's own rule, services/service-com-amazonaws-dlm-dlm-20180112/src/state/target_resource_type.rs:13-18:
"Several configuration members are documented for policies that target ONE of those two resource types only, so a request stating one for a policy that targets the other states a configuration the service cannot apply — the \"invalid parameters\" the write operations model `InvalidRequestException` for. It is refused before anything is stored, so no stored policy carries a member the resource type it targets admits nowhere and no read reports one."

The member list that rule is applied to, target_resource_type.rs:91-112 — four entries, none of them an `Exclusions` member:
```
const TARGET_RESTRICTED_MEMBERS: [TargetRestrictedMember; 4] = [
    TargetRestrictedMember { name: "VariableTags", admitted_target: INSTANCE_TARGET, ... },
    TargetRestrictedMember { name: "ArchiveRule", admitted_target: VOLUME_TARGET, ... },
    TargetRestrictedMember { name: "ExcludeBootVolume", admitted_target: INSTANCE_TARGET, ... },
    TargetRestrictedMember { name: "ExcludeDataVolumeTags", admitted_target: INSTANCE_TARGET, ... },
];
```

And the reason it judges no default policy, target_resource_type.rs:161-168:
```
pub fn target_resource_type_violation(
    details: Option<&PolicyDetails>,
) -> Option<InvalidRequestException> {
    let details = details?;
    let targets = details.resource_types.as_deref()?;
```
beside the stated reason at target_resource_type.rs:42-46: "A configuration that states no target has none to judge a member against — ... a default policy states `PolicyDetails$ResourceType` and carries no schedules or parameters at all ... — so nothing is refused for one." That reason covers `Schedules` and `Parameters`; it does not cover `Exclusions`, which IS a `[Default policies only]` member with per-kind restrictions.

The write paths store it unjudged — src/ops/create_lifecycle_policy.rs:465-473:
```
        let configuration = crate::state::DefaultPolicyConfiguration {
            resource_type: default_policy_kind,
            ...
            exclusions: input.exclusions,
        }
```
and create_lifecycle_policy.rs:502 `exclusions: configuration.exclusions,`; src/ops/update_lifecycle_policy.rs:444-446:
```
        if let Some(exclusions) = input.exclusions {
            policy.exclusions = Some(exclusions);
        }
```

Test coverage: the invalid combination is exercised today and asserted to SUCCEED — tests/all/default_policy_configuration_precedence.rs:56 `const DEFAULT_POLICY_TYPE: &str = "VOLUME";` with tests/all/default_policy_configuration_precedence.rs:320-325:
```
        Replacement {
            case: "exclusions by instance tag",
            member: "Exclusions",
            created: json!({"ExcludeVolumeTypes": ["gp2"]}),
            updated: json!({"ExcludeTags": [{"Key": "backup", "Value": "never"}]}),
        },
```
No comment or test in the crate states a reason for admitting an EBS-backed-AMI-only member on an EBS-snapshot default policy. The valid-combination cases (tests/all/lifecycle_policy_crud.rs:1013-1027 `VOLUME` + `ExcludeBootVolumes`, tests/all/default_policy_configuration_one_place_each.rs:52,244-245 `VOLUME` + `ExcludeBootVolumes`/`ExcludeVolumeTypes`) are unaffected. The analogous custom-policy refusals are covered by tests/all/policy_configuration_validation.rs:1128-1205.

## Steps to Reproduce

1. POST /policies with `DefaultPolicy: "INSTANCE"` (a default policy for EBS-backed AMIs), a valid `ExecutionRoleArn`, `Description`, `State: "ENABLED"`, and `Exclusions: {"ExcludeVolumeTypes": ["gp2"]}` — a member documented `[Default policies for EBS snapshots only]`.
2. Observe HTTP 200 and a minted PolicyId instead of the `InvalidRequestException` the crate answers for a member the policy's resource type does not admit.
3. GET /policies/{PolicyId} and observe `/PolicyDetails/Exclusions/ExcludeVolumeTypes` reported on an `INSTANCE` default policy.
4. Mirror case: POST /policies with `DefaultPolicy: "VOLUME"` and `Exclusions: {"ExcludeTags": [{"Key": "backup", "Value": "never"}]}` — a member documented `[Default policies for EBS-backed AMIs only]` — and observe HTTP 200.
5. Same through the update path: create a `VOLUME` default policy, then PATCH /policies/{PolicyId} with `{"Exclusions": {"ExcludeTags": [{"Key": "backup", "Value": "never"}]}}`; observe HTTP 200 (this is the path tests/all/default_policy_configuration_precedence.rs:320-325 already exercises).
6. Compare with the custom-policy side: a custom snapshot policy targeting `VOLUME` stating `Parameters.ExcludeBootVolume` is refused 400 `InvalidRequestException` by `target_resource_type_violation`.
