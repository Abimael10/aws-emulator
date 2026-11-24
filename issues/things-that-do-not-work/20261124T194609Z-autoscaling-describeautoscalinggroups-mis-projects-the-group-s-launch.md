Issue-Source: trace-sweep-divergence
Issue-Trace: asglf-mixed-instances-policy-refusals
Issue-Step: task#16
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: asglf-mixed-instances-policy-refusals autoscaling-asgeni-traj-create-group-validates-template-network-interfaces autoscaling-asghc-traj-launch-template-version-and-reference-mistakes autoscaling-asghc-traj-unlaunchable-template-refused-until-fixed-version
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|DescribeAutoScalingGroups|2xx->2xx|$.DescribeAutoScalingGroupsResponse.DescribeAutoScalingGroupsResult.AutoScalingGroups.member.MixedInstancesPolicy.InstancesDistribution.OnDemandAllocationStrategy
Issue-Key: autoscaling|DescribeAutoScalingGroups|2xx->2xx|$.DescribeAutoScalingGroupsResponse.DescribeAutoScalingGroupsResult.AutoScalingGroups.member.MixedInstancesPolicy.LaunchTemplate.Overrides.member
Issue-Key: autoscaling|DescribeAutoScalingGroups|2xx->2xx|$.DescribeAutoScalingGroupsResponse.DescribeAutoScalingGroupsResult.AutoScalingGroups.member.LaunchTemplate.Version
Issue-Commit: 6df4780ed6263f6fd5cc79bc833a1ad99a020747

# autoscaling: DescribeAutoScalingGroups mis-projects the group's launch-template version and mixed-instances policy (2 causes)

> _Note: this issue was identified against the code base at commit `6df4780ed6263f6fd5cc79bc833a1ad99a020747`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAutoScalingGroups

## Description
DescribeAutoScalingGroups answers 200 but the group it projects does not match the group the accepted CreateAutoScalingGroup/UpdateAutoScalingGroup + launch-template calls actually configured. Two distinct causes, both in the group's stored launch configuration and its DescribeAutoScalingGroups projection. Neither is covered by an existing open issue: 20261022T112459Z-autoscaling-group-round-trip-create-skips-min-max.md owns the group-default/VPCZoneIdentifier projection fields, and 20261102T114505Z-autoscaling-createautoscalinggroup-and-updateautoscalinggroup-launch-template-and-argument-validation.md owns the create/update *refusals*; this issue is about the values a group reports after an ACCEPTED call.

Cause 1 of 2: the mixed-instances policy a described group reports is not the one the accepted CreateAutoScalingGroup supplied (fixture asglf-mixed-instances-policy-refusals, step task#16).
AWS: Smithy (com.amazonaws.autoscaling) models MixedInstancesPolicy.LaunchTemplate -> LaunchTemplate.Overrides as a LIST of LaunchTemplateOverrides, and each override carries EITHER InstanceType OR InstanceRequirements — LaunchTemplateOverrides.InstanceRequirements documents "If you specify InstanceRequirements, you can't specify InstanceType". Smithy InstancesDistribution.OnDemandAllocationStrategy documents the default as `prioritized` for Auto Scaling groups that do NOT specify InstanceRequirements, and `lowest-price` only for groups that DO. So a group created with two InstanceType-only overrides must describe back with both overrides, in the order supplied, and with OnDemandAllocationStrategy `prioritized` when the request omitted it. The recording shows exactly that: Overrides with two members, InstanceType `t3.micro` then InstanceType `t3a.micro`, and OnDemandAllocationStrategy `prioritized`.
Emulator: the described group carries a SINGLE override object, and that object carries InstanceType together with InstanceRequirements (VCpuCount.Min / MemoryMiB.Min) — a combination AWS rejects and that the accepted request did not contain — and OnDemandAllocationStrategy `lowest-price`. Hypothesis: the group retains the mixed-instances policy of an earlier attempt in the trajectory (the InstanceRequirements-style policy) instead of overwriting it with the accepted request's policy, and the allocation-strategy default is a hard-coded constant rather than being derived from whether the overrides specify InstanceRequirements.
Why: a client that reads back its group to confirm the configuration it just applied sees one instance type where it configured two, so weighted/multi-type capacity planning and any describe-then-update round trip operate on the wrong override list; it also sees an allocation strategy it never set, and a policy shape (InstanceType + InstanceRequirements in one override) that AWS would never return and that a strict SDK/validator can refuse.
Fix: the CreateAutoScalingGroup/UpdateAutoScalingGroup handler's mixed-instances-policy state write (replace the stored policy wholesale, including the full Overrides list, and derive the OnDemandAllocationStrategy default from the presence of InstanceRequirements) and the DescribeAutoScalingGroups serializer for MixedInstancesPolicy.LaunchTemplate.Overrides, which must emit one `member` per override.
Traces: asglf-mixed-instances-policy-refusals#task#16.

Cause 2 of 2: a described group's LaunchTemplate.Version is stale — one version behind the version the group is configured to use (fixture autoscaling-asgeni-traj-create-group-validates-template-network-interfaces, step task#10).
AWS: Smithy LaunchTemplateSpecification.Version is the version number, `$Latest` or `$Default`, and DescribeAutoScalingGroups reports the version the group resolves for launches (for `$Latest`, the newest existing version of the template). After an EC2 CreateLaunchTemplateVersion adds a further version and the group is pointed at it, the recorded response reports LaunchTemplate.Version `3`.
Emulator: the same step reports LaunchTemplate.Version `2` — the version in effect before the most recent CreateLaunchTemplateVersion/UpdateAutoScalingGroup — in 4 steps across 3 fixtures. Hypothesis: the group's stored launch-template reference is not refreshed when a new template version is created (for a `$Latest` reference) or when an update re-points it, so the projection lags by one version.
Why: a client cannot tell which launch-template version its group will launch from; a deploy that bumps a template version and verifies the group picked it up reads a stale number and either loops or concludes the roll-forward failed, and tooling that feeds the described version into DescribeLaunchTemplateVersions inspects the wrong version's data.
Fix: the launch-template reference stored by CreateAutoScalingGroup/UpdateAutoScalingGroup and its resolution on read in the DescribeAutoScalingGroups projection (resolve `$Latest`/`$Default` against the template's current versions at describe time instead of caching a version number).
Traces: autoscaling-asgeni-traj-create-group-validates-template-network-interfaces#task#10, autoscaling-asghc-traj-launch-template-version-and-reference-mistakes#task#11, autoscaling-asghc-traj-unlaunchable-template-refused-until-fixed-version#task#10, autoscaling-asghc-traj-unlaunchable-template-refused-until-fixed-version#task#12.

## Evidence
Cause 1 — asglf-mixed-instances-policy-refusals#task#16, DescribeAutoScalingGroups, status 200 expected and 200 actual:
  $...AutoScalingGroups.member.MixedInstancesPolicy.LaunchTemplate.Overrides.member
    expected: [{"InstanceType": "t3.micro"}, {"InstanceType": "t3a.micro"}]  (two list members)
    actual:   {"InstanceType": "t3.micro", "InstanceRequirements": {"VCpuCount": {"Min": "1"}, "MemoryMiB": {"Min": "512"}}}  (one member, InstanceType AND InstanceRequirements together)
  $...AutoScalingGroups.member.MixedInstancesPolicy.InstancesDistribution.OnDemandAllocationStrategy
    expected: 'prioritized'   actual: 'lowest-price'
Smithy: InstancesDistribution.OnDemandAllocationStrategy — 'prioritized' is "the default value for Auto Scaling groups that don't specify InstanceRequirements"; 'lowest-price' is "the default value for Auto Scaling groups that specify InstanceRequirements". LaunchTemplateOverrides.InstanceRequirements — "If you specify InstanceRequirements, you can't specify InstanceType". MixedInstancesPolicy.LaunchTemplate.Overrides is a list.

Cause 2 — autoscaling-asgeni-traj-create-group-validates-template-network-interfaces#task#10, DescribeAutoScalingGroups, status 200 expected and 200 actual:
  $...AutoScalingGroups.member.LaunchTemplate.Version  expected: '3'   actual: '2'
Same divergence on autoscaling-asghc-traj-launch-template-version-and-reference-mistakes#task#11, autoscaling-asghc-traj-unlaunchable-template-refused-until-fixed-version#task#10 and #task#12 (4 steps, 3 fixtures).
Smithy: LaunchTemplateSpecification.Version — version number, $Latest or $Default; $Latest selects the latest version of the launch template.
(The concrete template/group/instance-type names above are this corpus's values; the requirement is the shape and the derivation rule, not those literals.)

## Steps to Reproduce
Cause 1: replay fixture asglf-mixed-instances-policy-refusals up to step task#16 — create a launch template, issue the refused mixed-instances CreateAutoScalingGroup variants, then the accepted CreateAutoScalingGroup whose MixedInstancesPolicy.LaunchTemplate.Overrides lists two InstanceType-only overrides (t3.micro, t3a.micro) and whose InstancesDistribution omits OnDemandAllocationStrategy, then call DescribeAutoScalingGroups for that group. Observe one Overrides member carrying InstanceType plus InstanceRequirements, and OnDemandAllocationStrategy 'lowest-price' instead of two Overrides members and 'prioritized'.

Cause 2: replay fixture autoscaling-asgeni-traj-create-group-validates-template-network-interfaces up to step task#10 (or autoscaling-asghc-traj-unlaunchable-template-refused-until-fixed-version up to task#10/#12) — create a launch template, add versions with EC2 CreateLaunchTemplateVersion until a third version exists, create/update the Auto Scaling group against it, then call DescribeAutoScalingGroups. Observe AutoScalingGroups.member.LaunchTemplate.Version reporting the previous version number (2) rather than the current one (3).
