Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.pseudo-parameters-and-no-value-resolve
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.pseudo-parameters-and-no-value-resolve|run#30|AssertionError|assert set(azs) == real_azs, (
Issue-Commit: 53ca3190ddccd906197b0d4d5e8474d7add13c5d

# cloudformation: Fn::GetAZs resolves to a static three-zone list instead of the region's real Availability Zones

> _Note: this issue was identified against the code base at commit `53ca3190ddccd906197b0d4d5e8474d7add13c5d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the CloudFormation intrinsic Fn::GetAZs is answered from a fixed, hard-coded zone table rather than from the live EC2 Availability-Zone state of the caller's region, so it returns fewer zones than the account actually has.

AWS: Fn::GetAZs returns the list of Availability Zones for the given region (documented as the zones of the region in which the account has a default subnet); it is derived reference data, never a constant. The authority is EC2 itself: ec2:DescribeAvailabilityZones (Filters region-name=<region>, state=available) for the region's zones, and ec2:DescribeSubnets with the default-for-AZ attribute for the account's default-subnet zones. The Smithy model describes no intrinsic functions, so the shape of the value is recorded behavior only: a list of well-formed zone names, all inside the caller's region, with no repeats, and Fn::GetAZs "" identical to Fn::GetAZs <region>.
Emulator: a stack whose Output is {"Fn::GetAZs": ""} resolves to a three-element list while EC2, queried in the same run by the same caller, reports four available zones for the region. The resolved list is a proper subset of the region's zones — it is a static table that does not track how many zones the region/account actually exposes. The earlier beats of the same check passed (AWS::Region, AWS::AccountId, AWS::Partition, AWS::URLSuffix, Fn::Base64, AWS::NoValue elision), so only the zone list diverges.
Why: every template that sizes or places resources from the zone list — a subnet per AZ, Fn::Select over Fn::GetAZs, an ASG's AvailabilityZones, Fn::Length of the list — is handed a shorter list than the account has. A deploy that AWS would spread over every zone silently lands in a subset, multi-AZ topologies come out with fewer members than the template intends, and a client comparing Fn::GetAZs with ec2:DescribeAvailabilityZones sees two different answers from the same emulator.
Fix: the CloudFormation intrinsic-function evaluator for Fn::GetAZs — resolve it at template-resolution time from the emulator's live EC2 zone/default-subnet state for the named (or implicit) region instead of a static zone table.
Traces: cloudformation.pseudo-parameters-and-no-value-resolve (assertion: "Fn::GetAZs ['us-west-2a', 'us-west-2b', 'us-west-2c'] must equal EC2's available zones for 'us-west-2': ['us-west-2a', 'us-west-2b', 'us-west-2c', 'us-west-2d']"). The specific zone names and the region are per-environment values, not part of the requirement; the requirement is that the two lists are equal.

Related: the open issue 20261107T201046Z-cloudformation-ec2-fn-getazs-is-a-static-zone.md reports the same static-table root cause from the EC2-paired suite (it does not carry this check's replay key, so this failure is not covered by it).

## Evidence
From the behavioral replay — cloudformation.pseudo-parameters-and-no-value-resolve [mismatch]:
  File "check_cloudformation_pseudo_parameters_and_no_value_resolve.py", line 178, in run
    assert set(azs) == real_azs, (...)
  AssertionError: Fn::GetAZs ['us-west-2a', 'us-west-2b', 'us-west-2c'] must equal EC2's available zones for 'us-west-2': ['us-west-2a', 'us-west-2b', 'us-west-2c', 'us-west-2d']

Expected: the comma-joined Output of {"Fn::GetAZs": ""}, split into zone names, equals the set of ZoneName values ec2:DescribeAvailabilityZones(Filters=[region-name=<region>, state=available]) returns.
Actual: the stack Output lists three zones; EC2 reports four available zones for the same region in the same run. Every other pseudo-parameter assertion in the check passed before this one.

## Steps to Reproduce
1. Replay the behavioral suite and re-run cloudformation.pseudo-parameters-and-no-value-resolve.
2. Manually: call ec2:DescribeAvailabilityZones with Filters region-name=<region>, state=available and note the ZoneName list. Then CreateStack with a template whose Outputs include {"Azs": {"Value": {"Fn::Join": [",", {"Fn::GetAZs": ""}]}}}, wait for CREATE_COMPLETE and read the Output. The two zone lists must be identical; the emulator's Output is a shorter, fixed list.
