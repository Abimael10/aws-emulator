Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.suspended-launch-process-blocks-launch
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.suspended-launch-process-blocks-launch|_assert_stays_false#19|Timeout|time.sleep(interval_s)
Issue-Commit: fdcbc9bed8e477940feb10c6ba4d4b725127ede6

# autoscaling: the suspended-Launch capacity scenario never reaches a verdict — a group whose Launch process is suspended and then resumed makes no observable progress inside the suite budget

> _Note: this issue was identified against the code base at commit `fdcbc9bed8e477940feb10c6ba4d4b725127ede6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SuspendProcesses

## Description
One net-new cause.

Cause 1 of 1 — the Launch-process suspension/resume capacity flow produces no observable transition, so neither the suppression nor the resume half of the contract is ever exercised.
Failing check id: autoscaling.suspended-launch-process-blocks-launch.

AWS: Smithy: SuspendProcesses and ResumeProcesses both take ScalingProcessQuery {AutoScalingGroupName (required, XmlStringMaxLen255, length 1..255), ScalingProcesses: ProcessNames} and return an empty body (output_shape Unit), with only ResourceInUse (400) / ResourceContention (500) modeled; SetDesiredCapacity takes SetDesiredCapacityType {AutoScalingGroupName (required), DesiredCapacity (required), HonorCooldown} and likewise returns Unit, with ScalingActivityInProgress (400) / ResourceContention (500) modeled. The state machine behind them is not in the model, so the contract here is recorded behavior only: while the 'Launch' process is suspended, DescribeAutoScalingGroups must report it under SuspendedProcesses, a SetDesiredCapacity that raises DesiredCapacity must be recorded on the group (DesiredCapacity == the requested value) and must create NO instances — the group's Instances list stays empty for as long as the suspension holds; after ResumeProcesses('Launch') the process disappears from SuspendedProcesses and the group then acts on the standing intent, launching instances until the group has DesiredCapacity members in LifecycleState 'InService'.
Emulator: the scenario reaches no verdict at all. The replayed check is cut by the harness with 'Check timed out after 60.0s' while polling the group's membership, so the emulator never produced the state the flow turns on: no observable transition of the group's Instances list (neither the required absence of a member while Launch is suspended followed by progress, nor a member reaching InService after the resume) was obtained inside the budget. Hypothesis: the capacity reconciliation loop does not act on the standing DesiredCapacity intent once the Launch process is resumed, so the group stays at zero members and the check's InService wait can never complete; the alternative — that reconciliation runs but the suspension gate is consulted late — would also leave this flow unverifiable. An emulator settles these transitions in-process, so a 60 s cap is ample; the check's own deadlines are 60-180 s.
Why: a client that uses SuspendProcesses('Launch') as a freeze (change windows, maintenance, cost freezes, blue/green cutovers) and then ResumeProcesses to let the recorded capacity intent take effect never converges: the group neither demonstrably withholds the launch nor performs it afterwards, so the freeze cannot be trusted and the thaw never delivers capacity. Automation that waits for DesiredCapacity members to reach InService after a resume hangs.
Fix: the autoscaling capacity reconciliation engine — the suspended-process gate on the launch path and the resumption of reconciliation toward a standing DesiredCapacity after ResumeProcesses — plus whatever per-transition wall-clock delay keeps group membership from settling in-process. Open issue 20261118T161732Z-autoscaling-never-advances-instance-refresh-terminate-hook-or.md (Cause 3) describes the capacity reconciliation loop never materializing SetDesiredCapacity effects and 20261118T161732Z-autoscaling-reconciliation-state-diverges-a-suspended-replaceunhealthy-still.md describes a suspended ReplaceUnhealthy still acting, but neither carries this check or the Launch-process suspend/resume pair it pins.
Traces: autoscaling.suspended-launch-process-blocks-launch.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout; service heading '### autoscaling'.

[mismatch] autoscaling.suspended-launch-process-blocks-launch
Error: Check timed out after 60.0s

The check's shape (from the replayed source): create a launch template and a group with MinSize=0, MaxSize=2, DesiredCapacity=0, HealthCheckGracePeriod=0 over two subnets; SuspendProcesses(ScalingProcesses=['Launch']) and assert 'Launch' appears in DescribeAutoScalingGroups[].SuspendedProcesses; SetDesiredCapacity(DesiredCapacity=1) and assert DescribeAutoScalingGroups[].DesiredCapacity == 1; assert the group's Instances list never becomes non-empty while the suspension holds; ResumeProcesses(ScalingProcesses=['Launch']) and assert 'Launch' is gone from SuspendedProcesses; then poll until the group has exactly 1 instance with LifecycleState == 'InService'.

Expected (recorded AWS behavior): the suspension is reported, the raised DesiredCapacity is recorded with no instance created, and after the resume one member reaches InService.
Actual (emulator): the body is killed at the 60 s cap with no assertion reached — the group's membership never moved into either of the two states the check distinguishes, so the suppression and the post-resume launch are both unobserved. Other autoscaling reads answer promptly in the same run (252/277 autoscaling checks pass), so the stall is in this reconciliation flow, not in request handling.

## Steps to Reproduce
1. Replay the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout; check autoscaling.suspended-launch-process-blocks-launch reports 'Check timed out after 60.0s'.
2. Manually: CreateLaunchTemplate with an Amazon Linux 2023 ImageId and InstanceType t3.micro, then CreateAutoScalingGroup with MinSize=0, MaxSize=2, DesiredCapacity=0, HealthCheckGracePeriod=0 and VPCZoneIdentifier naming two subnets.
3. SuspendProcesses(AutoScalingGroupName=<group>, ScalingProcesses=['Launch']); DescribeAutoScalingGroups must list 'Launch' in SuspendedProcesses.
4. SetDesiredCapacity(AutoScalingGroupName=<group>, DesiredCapacity=1); DescribeAutoScalingGroups must report DesiredCapacity 1 and an empty Instances list, and must keep Instances empty while the suspension holds.
5. ResumeProcesses(AutoScalingGroupName=<group>, ScalingProcesses=['Launch']); DescribeAutoScalingGroups must no longer list 'Launch' as suspended, and must then converge to exactly one instance with LifecycleState 'InService'. Observe that step 5 never converges (and step 4's membership state never becomes observable) within the client's budget.
