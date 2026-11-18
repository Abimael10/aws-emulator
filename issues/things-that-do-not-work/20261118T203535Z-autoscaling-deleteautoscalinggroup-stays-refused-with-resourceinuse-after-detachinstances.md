Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.delete-auto-scaling-group-with-target-group-attached-rejected
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: auto-scaling|DeleteAutoScalingGroup|ResourceInUse
Issue-Commit: e1f71a84fd686660699f460f533bddedc75c2265

# autoscaling: DeleteAutoScalingGroup stays refused with ResourceInUse after DetachInstances(ShouldDecrementDesiredCapacity=true), so a group can never be deleted without ForceDelete (1 cause)

> _Note: this issue was identified against the code base at commit `e1f71a84fd686660699f460f533bddedc75c2265`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAutoScalingGroup

## Description
Cause 1 of 1: a non-force DeleteAutoScalingGroup remains refused with ResourceInUse after the group's last member has been detached with the desired-capacity decrement, so the teardown sequence AWS documents never converges. Failing check id: autoscaling.delete-auto-scaling-group-with-target-group-attached-rejected.

AWS: Smithy (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model) models DeleteAutoScalingGroup with errors ResourceInUseFault (errorKind client, httpError 400, awsQueryError code `ResourceInUse`) and ScalingActivityInProgressFault (400, awsQueryError code `ScalingActivityInProgress`), and the operation's own documentation states the remedy: "To remove instances from the Auto Scaling group before deleting it, call the DetachInstances API with the list of instances and the option to decrement the desired capacity. This ensures that Amazon EC2 Auto Scaling does not launch replacement instances." So AWS refuses the non-force delete ONLY while the group still holds instances or still asks for capacity, with the fixed sentence "You cannot delete an AutoScalingGroup while there are instances or pending Spot instance request(s) still in the group." (no variable parts — it names neither the group nor an instance). Recorded behavior: after DetachInstances(ShouldDecrementDesiredCapacity=true) on the group's only member and that instance's termination in EC2, the IDENTICAL DeleteAutoScalingGroup call succeeds; any activity still settling at that instant is reported with the transient code `ScalingActivityInProgress` (or `ResourceContention`), never with `ResourceInUse`.

Emulator: the first, expected refusal is correct (code and message both match), but the positive-control delete issued after the detach and the EC2 termination is still answered HTTP 400 / `ResourceInUse` with that same in-use sentence: botocore.errorfactory.ResourceInUseFault: An error occurred (ResourceInUse) when calling the DeleteAutoScalingGroup operation: You cannot delete an AutoScalingGroup while there are instances or pending Spot instance request(s) still in the group. At that moment DescribeAutoScalingGroups reports no InService member for the group, so the group's own in-use accounting (its instance list and/or its desired capacity) disagrees with the membership it reports: the released member — or the capacity the decrement was supposed to take off — is still counted by the delete's in-use test. Because `ResourceInUse` is not a transient code, a client that retries only through `ScalingActivityInProgress`/`ResourceContention` (as the recording does, for 300 s) never gets past it.

Why: a client that follows AWS's documented teardown — DetachInstances with the decrement, then DeleteAutoScalingGroup — can never delete the group. The delete loops on a 400 forever and the only way out is ForceDelete, which has different semantics (it terminates instances and deletes the warm pool). Any automation, CloudFormation-like deleter or test fixture that tears a group down this way leaks the group permanently.

Fix: the in-use test in DeleteAutoScalingGroup's handler (services/.../src/ops/delete_auto_scaling_group.rs) together with the membership/capacity bookkeeping DetachInstances leaves behind (src/ops/detach_instances.rs and the detach/materialize path in src/state) — a member the detach released must stop being counted as an instance of the group, and the decremented desired capacity must be what the delete reads; a change still settling belongs under the modeled `ScalingActivityInProgress`, not `ResourceInUse`.

Not covered by any open issue: 20261107T081710Z-autoscaling-detachinstances-stub.md is a scaffold-stub issue for DetachInstances, which now answers (the detach in this check succeeds and the suite did not mark this check as blocked by a stub), and no open issue carries this check's replay key.

Traces: autoscaling.delete-auto-scaling-group-with-target-group-attached-rejected (the refusal is raised at the check's positive-control delete, after the detach and the EC2 termination).

## Evidence
Expected (recorded AWS): after `DetachInstances(AutoScalingGroupName=<grp>, InstanceIds=[<id>], ShouldDecrementDesiredCapacity=True)` and `TerminateInstances`, `DeleteAutoScalingGroup(AutoScalingGroupName=<grp>)` returns 200 and the group disappears from DescribeAutoScalingGroups; a still-settling activity would answer the transient 400 `ScalingActivityInProgress`.
Actual (emulator): the same call answers HTTP 400, Error.Code `ResourceInUse`, Error.Message "You cannot delete an AutoScalingGroup while there are instances or pending Spot instance request(s) still in the group." (fixed text, no variable parts) — raised as botocore.errorfactory.ResourceInUseFault by the check's positive control, while DescribeAutoScalingGroups shows no InService member for the group.
The earlier, intended refusal in the same check (delete while the attached instance is InService, with a target group attached) matched AWS exactly in both code and message, so only the post-detach state is wrong.

## Steps to Reproduce
1. CreateLaunchTemplate, then CreateAutoScalingGroup(MinSize=0, MaxSize=1, DesiredCapacity=0) in one AZ/subnet.
2. CreateTargetGroup and AttachLoadBalancerTargetGroups on the group.
3. RunInstances one instance, wait for `running`, AttachInstances it to the group, wait until DescribeAutoScalingGroups reports it InService.
4. DeleteAutoScalingGroup (no ForceDelete) -> correctly refused 400 ResourceInUse with the in-use sentence; the group still exists.
5. DetachInstances(InstanceIds=[<id>], ShouldDecrementDesiredCapacity=True), then TerminateInstances on that id, and wait until the group reports no InService member.
6. DeleteAutoScalingGroup (no ForceDelete) again, retrying only through ScalingActivityInProgress/ResourceContention: the emulator keeps answering 400 ResourceInUse (AWS deletes the group).
Replay: autoscaling.delete-auto-scaling-group-with-target-group-attached-rejected
