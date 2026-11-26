Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected
Issue-Key: autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected|run#46|AssertionError|assert b_after == b1_ids, (
Issue-Commit: 9f7bf23473ff024f6e39d27cef35bcce93712a4b

# autoscaling__cloudformation__ec2: a rolled-back AutoScalingReplacingUpdate replaces the ORIGINAL group's instances instead of leaving them untouched

> _Note: this issue was identified against the code base at commit `9f7bf23473ff024f6e39d27cef35bcce93712a4b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: a CloudFormation stack update that is REJECTED under `UpdatePolicy: AutoScalingReplacingUpdate: WillReplace: true` rolls the stack back correctly at the surface, but the surviving ORIGINAL Auto Scaling group no longer holds the instances it held before the update.

AWS: recorded behavior only (CloudFormation UpdatePolicy semantics are not expressed in the Smithy model; the Auto Scaling/CloudFormation Smithy models carry no trait for AutoScalingReplacingUpdate). AWS documents AutoScalingReplacingUpdate/WillReplace as: CloudFormation creates a WHOLE NEW Auto Scaling group alongside the existing one, waits for the new group's CreationPolicy ResourceSignal verdict, and only on SUCCESS does it switch the logical resource to the new group and delete the old one. On a FAILURE signal from the new group's instances, the update is rejected: the stack ends UPDATE_ROLLBACK_COMPLETE, the logical resource keeps pointing at the ORIGINAL group, the replacement group and all of its instances are deleted, and the original group is NOT touched at all — the same instance ids that were InService before the update are still InService after it, still running, still carrying the pre-update UserData. A rollback is a no-op on the retained resource.

Emulator: the surface of the rollback is right — the stack reaches UPDATE_ROLLBACK_COMPLETE, the logical resource resolves back to the original group name, exactly one replacement group appears in the events, a FAILURE-signal reason is present, and only the original group remains — but DescribeAutoScalingGroups on the retained original group reports a DIFFERENT InService instance set than before the update: before=['i-013f486e98684db4b', 'i-013f486faa5deaa48'], after=['i-013f489332aa52537', 'i-013f48951927ad369'] (ids shown are from one run and are per-run values; the requirement is set equality of the pre-update and post-rollback InService id sets, not these literals). The emulator recycled/relaunched the retained group's members as part of unwinding the update.

Why: a client that uses WillReplace precisely because it wants a rejected update to be harmless loses its running fleet anyway. Every id a caller recorded before the update (for log correlation, for target-group registration, for in-guest state, for EIP/ENI attachments) is stale, and anything the original instances held in memory or on instance store is gone. A rollback that silently reprovisions the resource it is supposed to retain makes AutoScalingReplacingUpdate strictly more dangerous than no update policy at all.

Fix: the CloudFormation stack-update/rollback engine's handling of the AutoScalingReplacingUpdate policy — the rollback path must discard only the replacement group and leave the retained original AWS::AutoScaling::AutoScalingGroup's state (its instance membership) completely untouched, rather than re-reconciling or recreating its capacity. Note the two existing open issues about this same scenario's EARLIER stages — 20261031T211733Z-autoscaling-cloudformation-ec2-a-stack-whose-aws-autoscaling.md (CreationPolicy ResourceSignal always ends CREATE_FAILED) and 20261110T124641Z-autoscaling-cloudformation-ec2-an-autoscalingreplacingupdate-willreplace-stack-update.md (such an update never settles) — neither covers this check, which now gets past both of those stages and fails on the retained group's membership.

Traces: autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected.

## Evidence
Check: autoscaling__cloudformation__ec2.deploy-autoscaling-1-replacing-update-swaps-group-failed-signal-replacement-rejected

Expected (AWS): after a FAILURE-signalled AutoScalingReplacingUpdate is rejected and the stack reaches UPDATE_ROLLBACK_COMPLETE, _in_service_ids(original_group) == the set captured before the update.

Actual (emulator):
  File "check_autoscaling__cloudformation__ec2_deploy_autoscaling_1_replacing_update_swaps_group_failed_signal_replacement_rejected.py", line 382, in run
    assert b_after == b1_ids
  AssertionError: the original group must keep its original instances through a rejected replacement: before=['i-013f486e98684db4b', 'i-013f486faa5deaa48'], after=['i-013f489332aa52537', 'i-013f48951927ad369']

All earlier assertions in the same check passed, so the divergence is isolated to the retained group's instance membership: stack status UPDATE_ROLLBACK_COMPLETE (correct), logical resource -> original group (correct), exactly one replacement group in events (correct), a FAILURE-signal event reason present (correct), only the original group still present (correct).

The instance ids above are per-run values and are not part of the requirement; the requirement is that the two sets are equal.

## Steps to Reproduce
1. Create a stack whose AWS::AutoScaling::AutoScalingGroup has DesiredCapacity=2, NO AutoScalingGroupName, a CreationPolicy ResourceSignal, and UpdatePolicy: AutoScalingReplacingUpdate: WillReplace: true. UserData calls `aws cloudformation signal-resource` with SignalExitCode=0.
2. Wait for CREATE_COMPLETE and record the group's physical id and its InService instance ids (DescribeAutoScalingGroups).
3. UpdateStack bumping the Generation parameter (which rewrites the launch template UserData, changing the group's LaunchTemplate property and forcing a replacement) AND setting SignalExitCode=1 so the replacement group's instances signal FAILURE.
4. Wait for the stack to settle; it reaches UPDATE_ROLLBACK_COMPLETE and the logical resource points back at the original group.
5. Call DescribeAutoScalingGroups on the original group again and compare its InService instance ids to the set recorded in step 2. They differ; AWS returns the same set.
6. Also observe the retained instances' state and UserData: AWS keeps them `running` with the generation-1 UserData.
