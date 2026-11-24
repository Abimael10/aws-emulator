Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.suspended-launch-process-blocks-launch
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.suspended-launch-process-blocks-launch|_assert_stays_false#19|Timeout|time.sleep(interval_s)
Issue-Commit: 223839154b06db1915840e543c0e30a4ec77053f

# autoscaling: a Launch-suspended group never completes the suspend / record-desired / resume-and-launch cycle

> _Note: this issue was identified against the code base at commit `223839154b06db1915840e543c0e30a4ec77053f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SuspendProcesses

## Description
One net-new root cause (the check carries no owning open issue and did not land on a scaffold stub).

Cause 1 of 1: the process-suspension cycle for the Launch process does not converge — suspending Launch, recording a raised DesiredCapacity, then resuming Launch never yields the in-service instance AWS produces, and the scenario runs out its budget instead of completing.

AWS: Auto Scaling models SuspendProcesses/ResumeProcesses as ScalingProcessQuery{AutoScalingGroupName (required), ScalingProcesses} with an empty (Unit) output — success is HTTP 200 carrying only SuspendProcessesResponse/ResponseMetadata. "Launch" is one of the suspendable process names. Documented behavior (https://docs.aws.amazon.com/autoscaling/ec2/userguide/as-suspend-resume-processes.html): while the Launch process is suspended the group still ACCEPTS and RECORDS capacity intent — SetDesiredCapacity returns 200 and DescribeAutoScalingGroups reports AutoScalingGroups.member.DesiredCapacity as the requested value — but the group creates NO instance, so AutoScalingGroups.member.Instances stays empty for as long as the suspension lasts, and SuspendedProcesses contains a member {ProcessName: "Launch", SuspensionReason: <reason text>}. After ResumeProcesses with ScalingProcesses=["Launch"], "Launch" is no longer present in SuspendedProcesses and the group's reconciliation then launches instances until the recorded DesiredCapacity is met, each reaching Instance.LifecycleState "InService" (LifecycleState enum). Fixed text in these projections is the member/enum names themselves ("Launch", "InService", "DesiredCapacity"); the variable parts are the group name from the request and the generated instance ids and capacity numbers from state.

Emulator: the replay of this scenario never reaches its final assertion — the suite ends it with "Check timed out after 60.0s" and no assertion of its own was raised, i.e. the emulator did not carry the group through suspend -> record desired capacity -> resume -> one InService instance inside the budget. The failure is a non-convergence of the Launch-suspension/reconciliation path rather than a single wrong field; the suite reports the same "Check timed out after 60.0s" shape for the other autoscaling reconciliation scenarios that do not settle.

Why: a client that uses process suspension as its safety switch — suspend Launch, stage capacity changes, resume — never gets its capacity back. DesiredCapacity reads as satisfied intent while no instance ever appears, so any wait for InService capacity after ResumeProcesses hangs forever, and automation that drains and re-arms a group (maintenance windows, blue/green staging, cost parking) deadlocks.

Fix: the SuspendProcesses/ResumeProcesses handlers' suspension bookkeeping and, principally, the Auto Scaling group reconciliation loop that fills DesiredCapacity — it must skip launches while "Launch" is in the group's suspended set and must resume filling the recorded DesiredCapacity promptly once "Launch" is resumed — together with the DescribeAutoScalingGroups projection of SuspendedProcesses/Instances/DesiredCapacity that the client polls.

Related but NON-COVERING open issues (they own other clusters of the suspend/standby surface from the wire corpus, not this behavioral non-convergence): 20261115T223640Z-autoscaling-auto-scaling-standby-suspend-process-and-group-lifecycle.md (SuspendProcesses refusal/process-name causes), 20261118T161732Z-autoscaling-reconciliation-state-diverges-a-suspended-replaceunhealthy-still.md (suspended ReplaceUnhealthy), 20261115T003554Z-autoscaling-ec2-elbv2-auto-scaling-a-group-with-addtoloadbalancer-suspended-still.md (suspended AddToLoadBalancer).

Traces: behavioral check autoscaling.suspended-launch-process-blocks-launch.

## Evidence
Behavioral suite run: 395/465 passed (84.9%); service autoscaling 238/277 (86%), 39 failed.

[mismatch] autoscaling.suspended-launch-process-blocks-launch
Error: Check timed out after 60.0s

The check performs, in order: CreateAutoScalingGroup(MinSize=0, MaxSize=2, DesiredCapacity=0) from a launch template; SuspendProcesses(ScalingProcesses=["Launch"]) and asserts "Launch" appears in DescribeAutoScalingGroups -> SuspendedProcesses; SetDesiredCapacity(DesiredCapacity=1) and asserts the group reports DesiredCapacity == 1; asserts no member appears in AutoScalingGroups.member.Instances while Launch stays suspended; ResumeProcesses(ScalingProcesses=["Launch"]) and asserts "Launch" is gone from SuspendedProcesses; then waits for exactly one Instance with LifecycleState == "InService".

Expected (AWS): every step above succeeds — 200 on suspend/resume, DesiredCapacity recorded as 1 with an empty Instances list during the suspension, and exactly one instance reaching LifecycleState "InService" after the resume.
Actual (emulator): the scenario never reaches that terminal state; the run is cut off by the check budget ("Check timed out after 60.0s") with none of the check's own assertions having fired, so the emulator does not complete the suspend -> record -> resume -> InService cycle. No per-run value (instance id, group name, timestamp) is part of the expectation.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the behavioral suite (simparity-behavioral-aws-v1) and select check autoscaling.suspended-launch-process-blocks-launch.
3. Or by hand against the emulator's autoscaling endpoint:
   a. CreateLaunchTemplate with a valid ImageId/InstanceType; CreateAutoScalingGroup(MinSize=0, MaxSize=2, DesiredCapacity=0, VPCZoneIdentifier=<two subnets>, HealthCheckGracePeriod=0).
   b. SuspendProcesses(AutoScalingGroupName=<group>, ScalingProcesses=["Launch"]); DescribeAutoScalingGroups and confirm SuspendedProcesses contains ProcessName "Launch".
   c. SetDesiredCapacity(AutoScalingGroupName=<group>, DesiredCapacity=1); DescribeAutoScalingGroups and confirm DesiredCapacity == 1 and Instances is empty.
   d. Poll DescribeAutoScalingGroups for ~60 s and confirm Instances STAYS empty while Launch is suspended.
   e. ResumeProcesses(AutoScalingGroupName=<group>, ScalingProcesses=["Launch"]); confirm "Launch" is no longer in SuspendedProcesses.
   f. Poll DescribeAutoScalingGroups and confirm exactly one Instance reaches LifecycleState "InService". Observe that the emulator does not reach this state.
