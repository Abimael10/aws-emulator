Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected
Issue-Key: autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected|run#36|AssertionError|assert updated[stack_b] == "UPDATE_ROLLBACK_COMPLETE", (
Issue-Commit: 090d1e6cd43e0643cbf490ec920c2df0995b313e

# A FAILURE resource signal from an AutoScalingReplacingUpdate replacement is ignored: the stack reaches UPDATE_COMPLETE instead of UPDATE_ROLLBACK_COMPLETE

> _Note: this issue was identified against the code base at commit `090d1e6cd43e0643cbf490ec920c2df0995b313e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: an UpdateStack that replaces an AWS::AutoScaling::AutoScalingGroup under `UpdatePolicy: AutoScalingReplacingUpdate: WillReplace: true` does not consume the replacement group's resource signals — instances that signal FAILURE are ignored and the update is committed.

AWS: Smithy (service-com-amazonaws-cloudformation-cloudformation): SignalResource takes StackName (required), LogicalResourceId (required), UniqueId (required, @length 1..64) and Status (required, enum ResourceSignalStatus with values SUCCESS and FAILURE). With UpdatePolicy.AutoScalingReplacingUpdate.WillReplace=true and a CreationPolicy.ResourceSignal on the group, CloudFormation creates a NEW Auto Scaling group for the update and holds the logical resource until the signal quorum arrives from the new group's instances. A SUCCESS quorum commits the replacement (the resource's PhysicalResourceId becomes the new group and the old group and its instances are deleted); a FAILURE signal fails the replacement, so the stack goes UPDATE_FAILED -> UPDATE_ROLLBACK_IN_PROGRESS -> UPDATE_ROLLBACK_COMPLETE, the replacement group and its instances are deleted, and the ORIGINAL group keeps its original instances and their original UserData. The stack events for the replacement group name the FAILURE signal as the failure reason. Recorded behavior only for the exact event wording; the requirement asserted here is the terminal status, the surviving physical id and the attributable failure reason.

Emulator: the update whose replacement instances all signal FAILURE settles in UPDATE_COMPLETE — "a replacement whose instances signal FAILURE must be rejected and the stack rolled back, but stack B ended in 'UPDATE_COMPLETE'. UPDATE_COMPLETE means the FAILURE signal was ignored. Reasons: []". DescribeStackEvents carries no failure reason at all (the reason list is empty), so nothing records that a FAILURE verdict was received, and the rejected replacement is committed rather than discarded.

Why: WillReplace exists precisely so a bad revision is caught before it takes over: the client expects the original group to still be serving and the replacement discarded. Here the signal gate is a no-op, so a launch template / UserData revision whose instances report failure is promoted into production, the original group and its instances are destroyed, and there is nothing left to roll back to. A deployment pipeline that treats UPDATE_COMPLETE as "the new fleet reported healthy" draws the opposite conclusion from the one the fleet reported.

Fix: the UpdatePolicy.AutoScalingReplacingUpdate path of the AWS::AutoScaling::AutoScalingGroup resource handler in the CloudFormation provisioner — signal accounting must consume SignalResource Status=FAILURE for the group's LogicalResourceId during the replacement, fail the resource, drive the stack into UPDATE_ROLLBACK_*, delete the replacement group and its instances, restore the logical resource's PhysicalResourceId to the original group, and emit a stack event naming the FAILURE signal.

Related but not covering: open issue 20261112T141009Z-autoscaling-cloudformation-ec2-2-net-new-causes-an.md (cause 2) covers the earlier step of this same check — WillReplace minting a new PhysicalResourceId — and carries that assertion as its replay key. That step now gets past, and the check fails further along on the FAILURE-signal rejection, which that issue does not own.

Traces: autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected.

## Evidence
autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected [mismatch]
  AssertionError: a replacement whose instances signal FAILURE must be rejected and the stack rolled back, but stack B ended in 'UPDATE_COMPLETE'. UPDATE_COMPLETE means the FAILURE signal was ignored. Reasons: []

Setup: two identical stacks, each an AWS::AutoScaling::AutoScalingGroup with DesiredCapacity 2, no AutoScalingGroupName (so the resource is replaceable), CreationPolicy.ResourceSignal, UpdatePolicy.AutoScalingReplacingUpdate {WillReplace: true}, and UserData that calls `aws cloudformation signal-resource --stack-name <stack> --logical-resource-id AutoScalingGroup --unique-id <instance-id> --status SUCCESS|FAILURE` according to a SignalExitCode parameter. Both are updated with a Generation bump (rewriting UserData -> new launch-template version -> the group's LaunchTemplate property changes); stack A's replacements signal SUCCESS, stack B's signal FAILURE.
  Expected (stack B): UPDATE_ROLLBACK_COMPLETE; the logical resource points back at the original group; exactly one replacement group appears in the stack events and at least one of its event reasons names the FAILURE signal; the original group still holds its original InService instances, still running the generation-1 UserData; the replacement group's instances are terminated at EC2.
  Actual (stack B): UPDATE_COMPLETE with an empty failure-reason list.
  (Stack A's SUCCESS leg is the positive control and is not the subject of this issue.)

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateStack a template whose AWS::AutoScaling::AutoScalingGroup has DesiredCapacity 2, no AutoScalingGroupName, a launch template, CreationPolicy.ResourceSignal {Count: 2} and UpdatePolicy.AutoScalingReplacingUpdate {WillReplace: true}, with UserData that signals the group's logical id SUCCESS. Wait for CREATE_COMPLETE and record the group's PhysicalResourceId and its InService instance ids.
3. UpdateStack with the same template but a parameter change that rewrites the launch template's UserData so the new instances call SignalResource with Status=FAILURE for LogicalResourceId AutoScalingGroup.
   Observed: the stack reaches UPDATE_COMPLETE and DescribeStackEvents reports no failure reason.
   Expected: UPDATE_FAILED -> UPDATE_ROLLBACK_COMPLETE; DescribeStackResources points back at the original group; a stack event for the replacement group names the FAILURE signal; the replacement group and its instances are deleted and the original group's instances are untouched.
4. Replay the check in trace_ids to confirm the fix.
