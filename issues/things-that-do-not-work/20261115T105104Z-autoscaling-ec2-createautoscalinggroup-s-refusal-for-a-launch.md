Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2.asgeni-asg-with-unknown-launch-template-rejected
Issue-Key: autoscaling__ec2.asgeni-asg-with-unknown-launch-template-rejected|run#12|AssertionError|assert got == ("ValidationError", want_message), (
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# autoscaling__ec2: CreateAutoScalingGroup's refusal for a launch template that does not exist is emulator-authored prose that names neither the template name nor its id

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAutoScalingGroup

## Description
ONE net-new root cause (Cause 1 of 1) on the Auto Scaling group-create validation path. The EC2 side of the scenario works: the VPC, the subnet and both launch templates are created, and one template is deleted before the call.

Cause 1 of 1 — the ValidationError message for a dangling LaunchTemplate reference is a single generic sentence instead of AWS's per-reference-form sentence that echoes the identifier the request used.

AWS: HTTP 400, Error.Code ValidationError. Smithy (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/..., awsQuery, version 2011-01-01) models CreateAutoScalingGroup errors as only AlreadyExistsFault, LimitExceededFault, ResourceContentionFault and ServiceLinkedRoleFailure, and LaunchTemplateSpecification's LaunchTemplateId/LaunchTemplateName are each documented 'Conditional: You must specify either a LaunchTemplateId or a LaunchTemplateName'; a dangling reference is therefore reported through the service-common ValidationError and the sentence is recorded behavior only. The message depends on WHICH identifier the request supplied: by name it is 'The specified launch template, with template name <launch-template-name>, does not exist.'; by id it is 'The specified launch template, with template ID <launch-template-id>, does not exist.' In both forms the only variable part is the identifier copied verbatim from the request's LaunchTemplateSpecification; the rest of each sentence, including the comma placement and the trailing full stop, is fixed text. No group is created.

Emulator: the right status and the right Code, but one message for both reference forms — 'At least one of the launch templates specified in the request does not exist.' It names no identifier at all and does not distinguish a by-name reference from a by-id one.

Why: ValidationError is the generic Auto Scaling client-error code, so the message is the only machine-visible explanation of what was wrong. A caller that submitted a MixedInstancesPolicy or a LaunchTemplate and wants to know WHICH identifier is dangling (the common case when a stack supplies both a name and an id, or several templates) gets a sentence that names nothing, and any assertion, log scraper or operator runbook keyed to AWS's wording treats a correctly refused create as an unrecognised failure.

Fix: the CreateAutoScalingGroup handler's launch-template resolution step must build the refusal message from the reference form it failed to resolve — the by-name sentence when input.LaunchTemplate.LaunchTemplateName was supplied and the by-id sentence when LaunchTemplateId was — rather than emitting one shared sentence; this belongs in that operation's validation/refusal builder, not in the awsQuery serializer.

Related but not covering: open issue 20261102T104402Z-autoscaling-ec2-5-net-new-root-causes-described.md carries this same check id and recorded, as its Cause 2, that the call SUCCEEDED (HTTP 200) and left a group behind. That part has since changed — the call is now refused with ValidationError — so what remains failing is only the message wording, which that issue's Cause 2 does not describe as a defect on its own. This issue is the net-new wording cause; it is not a re-report of the accept-anyway behaviour.

Traces: autoscaling__ec2.asgeni-asg-with-unknown-launch-template-rejected.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against this checkout (emulator serving; every EC2 setup call in the check succeeded).

[mismatch] autoscaling__ec2.asgeni-asg-with-unknown-launch-template-rejected — AssertionError raised at the check's `assert got == ("ValidationError", want_message)`:

  a group referencing deleted template {'LaunchTemplateName': '<lt-name>'} answered
  ('ValidationError', 'At least one of the launch templates specified in the request does not exist.'),
  wanted ('ValidationError', 'The specified launch template, with template name <lt-name>, does not exist.')

The check drives two cases against the SAME deleted template, so the requirement is a pair:
  - LaunchTemplate={'LaunchTemplateName': <name>} -> ('ValidationError', 'The specified launch template, with template name <name>, does not exist.')
  - LaunchTemplate={'LaunchTemplateId': <lt-id>}  -> ('ValidationError', 'The specified launch template, with template ID <lt-id>, does not exist.')
It fails on the first case, so the by-id sentence is not yet observed; both must be served.

Message anatomy — fixed text: 'The specified launch template, with template name ' / 'The specified launch template, with template ID ' and ', does not exist.' Variable parts: the launch-template NAME or the launch-template ID, taken from the request. The concrete name and lt- id in the replay are per-run generated values and are not part of the requirement.

The check additionally asserts that the refused create leaves nothing behind (DescribeAutoScalingGroups for the name returns []) and that the identical call against a live template succeeds; neither of those assertions is reached, so only the message is known to diverge.

Smithy grounding: CreateAutoScalingGroup errors = AlreadyExistsFault, LimitExceededFault, ResourceContentionFault, ServiceLinkedRoleFailure (no launch-template error is modelled), so the refusal rides the service-common ValidationError and its sentence is recorded behavior only.

## Steps to Reproduce
1) Build and boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1, check autoscaling__ec2.asgeni-asg-with-unknown-launch-template-rejected.
2) ec2 CreateVpc(10.91.0.0/16) + CreateSubnet; ec2 CreateLaunchTemplate(LaunchTemplateName=<name>, LaunchTemplateData={ImageId:<AL2023 ami>, InstanceType:'t3.micro'}), note the returned LaunchTemplateId, then ec2 DeleteLaunchTemplate(LaunchTemplateId=<that id>).
3) autoscaling CreateAutoScalingGroup(AutoScalingGroupName=<new name>, MinSize=0, MaxSize=1, DesiredCapacity=0, LaunchTemplate={'LaunchTemplateName': <deleted name>}, VPCZoneIdentifier=<subnet>). Expect HTTP 400, Error.Code 'ValidationError', Error.Message 'The specified launch template, with template name <deleted name>, does not exist.'
4) Repeat the same CreateAutoScalingGroup with LaunchTemplate={'LaunchTemplateId': <deleted lt-id>}. Expect HTTP 400, Error.Code 'ValidationError', Error.Message 'The specified launch template, with template ID <deleted lt-id>, does not exist.'
5) autoscaling DescribeAutoScalingGroups(AutoScalingGroupNames=[<name>]) must return AutoScalingGroups == [], and the identical create against a LIVE launch template must succeed.
