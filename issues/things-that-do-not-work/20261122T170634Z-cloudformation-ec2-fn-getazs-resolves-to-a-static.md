Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2.getazs-resolves-to-and-tracks-the-default-subnet-set
Issue-Key: cloudformation__ec2.getazs-resolves-to-and-tracks-the-default-subnet-set|run#12|AssertionError|assert before == baseline_azs, (
Issue-Commit: 8896a828822ada8fe26405eeb13d1bd63d2626fa

# cloudformation__ec2: Fn::GetAZs resolves to a static three-zone list that is not the account's default-subnet AZ set (baseline divergence)

> _Note: this issue was identified against the code base at commit `8896a828822ada8fe26405eeb13d1bd63d2626fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the value CloudFormation resolves for Fn::GetAZs is a fixed three-element zone table, not a projection of the Availability Zones in which the account holds a default subnet. Failing check: cloudformation__ec2.getazs-resolves-to-and-tracks-the-default-subnet-set.

NOTE ON AN EXISTING ISSUE: the open issue 20261107T201046Z-cloudformation-ec2-fn-getazs-is-a-static-zone.md names this same check, but it is keyed on the LATER assertion of the check (`assert mutated == expected_after`) — that the resolved list TRACKS a default subnet being deleted. It does not cover this failure: on this replay the check never gets that far, because the very first, BASELINE assertion (`assert before == baseline_azs`) already fails — the baseline resolution itself disagrees with the account's default-subnet set. That earlier divergence is a distinct cause and is what this issue reports.

AWS: Fn::GetAZs returns, as a list, the Availability Zones of the named region in which the ACCOUNT holds a default subnet. The authority for that set is ec2:DescribeSubnets — Smithy: ec2 Subnet.DefaultForAz, "Indicates whether this is the default subnet for the Availability Zone" — so the resolved list must equal exactly the set of zones whose subnets report DefaultForAz=true, in the account and region the stack is created in. Fn::GetAZs "" and Fn::GetAZs <region> resolve identically, and the result is a list so Fn::Select[0] yields its first element. CloudFormation intrinsics are not described by any Smithy model, so this is recorded behavior only for the intrinsic itself; the member it must agree with is modeled.

Emulator: the resolved list has one fewer zone than the account actually has default subnets in, and is not derived from EC2 state at all. In the recorded run ec2:DescribeSubnets(default-for-az=true) reported four zones while the template resolved three — and the region itself also has four zones, so the value is neither the account's default-subnet set nor the region's zone list but a static table. The zone names and the count are per-environment values; the requirement is the equality between the resolved list and the DescribeSubnets default-for-az zone set, whatever that set is.

Why: every template that sizes or places resources from Fn::GetAZs — a subnet-per-AZ loop, Fn::Select over the zone list, an Auto Scaling group's AvailabilityZones — is handed a zone list that does not match where the account can actually place default-subnet-backed resources. A client gets fewer zones than AWS would give it (so a stack silently under-provisions, or an Fn::Select index that is valid on AWS is out of range), and a client cannot discover the discrepancy from the API.

Fix: the CloudFormation intrinsic-function evaluator for Fn::GetAZs — resolve it at template-resolution time from the live EC2 default-subnet state for the stack's region, rather than from a static zone table.

Traces: cloudformation__ec2.getazs-resolves-to-and-tracks-the-default-subnet-set.

## Evidence
Failing check cloudformation__ec2.getazs-resolves-to-and-tracks-the-default-subnet-set [mismatch], at check_cloudformation__ec2_getazs_resolves_to_and_tracks_the_default_subnet_set.py line 65, `assert before == baseline_azs`:

  AssertionError: Fn::GetAZs must return exactly the AZs where this account has a DEFAULT SUBNET. ec2 DescribeSubnets(defaultForAz=true) says ['us-west-2a', 'us-west-2b', 'us-west-2c', 'us-west-2d']; the template resolved ['us-west-2a', 'us-west-2b', 'us-west-2c']. For reference the region has ['us-west-2a', 'us-west-2b', 'us-west-2c', 'us-west-2d'] -- a target returning the region's AZ list instead of the account's default-subnet AZs fails here

Expected: the list a stack resolves for Fn::GetAZs equals the zone set ec2:DescribeSubnets reports with DefaultForAz=true. Actual: a three-element list that matches neither that set nor the region's zone list. The specific zone names and the counts (4 vs 3) are values of this environment; the fixed requirement is the set equality.

## Steps to Reproduce
1. git_clone, then run the behavioral suite so the emulator is serving.
2. Re-replay cloudformation__ec2.getazs-resolves-to-and-tracks-the-default-subnet-set.
3. Manually: call ec2:DescribeSubnets and collect the AvailabilityZone of every subnet with DefaultForAz=true. Then CreateStack with a template whose Outputs are {"Fn::GetAZs": ""}, {"Fn::GetAZs": "<region>"} and {"Fn::Select": [0, {"Fn::GetAZs": ""}]}, wait for CREATE_COMPLETE and read the Outputs.
4. Compare the resolved zone list with the DescribeSubnets default-for-az zone set — they must be equal; the emulator returns a shorter, static list.
