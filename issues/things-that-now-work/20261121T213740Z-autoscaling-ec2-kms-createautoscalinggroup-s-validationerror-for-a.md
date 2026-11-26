Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__kms.asglf-rebuilding-template-and-group-rejected-as-duplicates
Issue-Key: autoscaling__ec2__kms.asglf-rebuilding-template-and-group-rejected-as-duplicates|run#22|AssertionError|assert message == f"Malformed launch template Id: {bogus_lt_id}", (
Issue-Commit: ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da

# autoscaling__ec2__kms: CreateAutoScalingGroup's ValidationError for a well-formed but nonexistent LaunchTemplateId reports a does-not-exist message instead of AWS's 'Malformed launch template Id: <id>'

> _Note: this issue was identified against the code base at commit `ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAutoScalingGroup

## Description
One net-new root cause under the suite's `### autoscaling__ec2__kms` heading.

Cause 1 of 1: the refusal message for a nonexistent launch template on CreateAutoScalingGroup is emulator-authored.
AWS: recorded behavior only — Auto Scaling validates the LaunchTemplate reference before it validates the group name, and refuses a well-formed but nonexistent LaunchTemplateId with the Query-protocol error code `ValidationError` and the message `Malformed launch template Id: <launch-template-id>`. The fixed text is `Malformed launch template Id: `; the only variable part is the LaunchTemplateId copied verbatim from the request's LaunchTemplate.LaunchTemplateId. The recording shows this message winning even when the AutoScalingGroupName in the same request already exists (a duplicate name on its own is refused with code `AlreadyExists` and the message `AutoScalingGroup by this name already exists - A group with the name <name> already exists`, which the emulator gets right — the assertions on that pair passed).
Emulator: the error code is correct (`ValidationError`) but the message is `The specified launch template, with template ID <launch-template-id>, does not exist.` — an EC2-style not-found sentence that Auto Scaling does not emit for this case, with different fixed text and a trailing period after an interpolated id.
Why: a client that classifies Auto Scaling refusals by their message text — the only discriminator available, since both the duplicate-name and bad-template cases arrive under a single generic `ValidationError` code in the Query protocol — cannot tell the two apart on the emulator, and user-facing surfacing of the refusal does not match AWS. It also hides that AWS deliberately calls the id "malformed" rather than "not found" here, so code written against the emulator's wording breaks on AWS.
Fix: the CreateAutoScalingGroup handler's launch-template validation message (in the Auto Scaling service crate's create_auto_scaling_group operation), keeping the existing ordering in which the launch-template check runs before the group-name check.
Traces: autoscaling__ec2__kms.asglf-rebuilding-template-and-group-rejected-as-duplicates.

## Evidence
Suite: simparity-behavioral-aws-v1 — 1444/2191 passed (65.9%). Service heading `### autoscaling__ec2__kms`: 7/11 (64%), 4 failed; this is the one failure the report did not mark "OWNED by" or "ALREADY FILED".

autoscaling__ec2__kms.asglf-rebuilding-template-and-group-rejected-as-duplicates [mismatch]:
  Call: autoscaling.create_auto_scaling_group(AutoScalingGroupName=<an existing group name>, MinSize=0, MaxSize=2, DesiredCapacity=0, LaunchTemplate={'LaunchTemplateId': 'lt-00000000000000000', 'Version': '$Latest'}, VPCZoneIdentifier=<subnet>)
  Expected: ValidationError with message 'Malformed launch template Id: lt-00000000000000000'
  Actual:   ValidationError with message 'The specified launch template, with template ID lt-00000000000000000, does not exist.'
  AssertionError: expected the exact malformed-template-id message, got 'The specified launch template, with template ID lt-00000000000000000, does not exist.'

The earlier assertions in the same check passed, so the emulator already gets right: the ClientToken replay returning the same LaunchTemplateId/LatestVersionNumber; the duplicate launch-template name under a fresh token returning InvalidLaunchTemplateName.AlreadyExistsException / 'Launch template name already in use.'; and the duplicate group name returning AlreadyExists / 'AutoScalingGroup by this name already exists - A group with the name <name> already exists'. Only the launch-template-id message diverges.

## Steps to Reproduce
Create a VPC 10.90.0.0/16 with a subnet, a KMS key and an EC2 launch template (CreateLaunchTemplate) whose data references it, then CreateAutoScalingGroup(AutoScalingGroupName=<name>, MinSize=0, MaxSize=1, DesiredCapacity=0, LaunchTemplate={'LaunchTemplateId': <real id>, 'Version': '1'}, VPCZoneIdentifier=<subnet>). Now call CreateAutoScalingGroup again with the SAME AutoScalingGroupName but LaunchTemplate={'LaunchTemplateId': 'lt-00000000000000000', 'Version': '$Latest'} — a well-formed id that does not exist. Read the error code and message: AWS answers code 'ValidationError' with message 'Malformed launch template Id: lt-00000000000000000' (the launch-template check wins over the duplicate-name check); the emulator answers the same code with 'The specified launch template, with template ID lt-00000000000000000, does not exist.' Confirm afterwards that DescribeAutoScalingGroups shows the original group's MinSize/DesiredCapacity/MaxSize unchanged.
