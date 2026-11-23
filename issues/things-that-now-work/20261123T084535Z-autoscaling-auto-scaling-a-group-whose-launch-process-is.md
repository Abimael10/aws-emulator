Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.suspended-launch-process-blocks-launch
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.suspended-launch-process-blocks-launch|_assert_stays_false#19|Timeout|time.sleep(interval_s)
Issue-Commit: dbc160e5ba09bc1337b7dfc453c49db2296d6587

# Auto Scaling: a group whose Launch process is suspended and then resumed never converges to an InService instance

> _Note: this issue was identified against the code base at commit `dbc160e5ba09bc1337b7dfc453c49db2296d6587`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ResumeProcesses

## Description
One net-new root cause in the autoscaling process-suspension/reconciliation plane (no open issue carries this check's replay key, and it is not blocked by a scaffold stub).

Cause 1 of 1: the suspend-Launch / raise-DesiredCapacity / resume-Launch round trip never reaches a verdict against the emulator.
AWS: Smithy (com.amazonaws.autoscaling#AutoScaling_2011_01_01): SuspendProcesses and ResumeProcesses take ScalingProcessQuery{AutoScalingGroupName (required), ScalingProcesses} and return Unit — success is 200 with an empty result body; their only modeled client error is ResourceInUseFault (400/ResourceInUse). SetDesiredCapacity takes SetDesiredCapacityType{AutoScalingGroupName (required), DesiredCapacity (required), HonorCooldown} and also returns Unit, with ScalingActivityInProgressFault (400) as its only modeled client error. DescribeAutoScalingGroups projects the group's SuspendedProcesses (ProcessName + SuspensionReason) and its Instances, each with a LifecycleState from the LifecycleState enum (Pending -> InService ...). Per AWS's documented suspend/resume semantics (https://docs.aws.amazon.com/autoscaling/ec2/userguide/as-suspend-resume-processes.html), while Launch is suspended a raised DesiredCapacity is RECORDED but no instance is created, and once Launch is resumed the group resumes reconciliation and fills the recorded desired capacity, the new member moving Pending -> InService within normal launch latency. Statuses/codes here are the modeled ones; the convergence requirement is the documented suspend/resume rule, not one recording's values.
Emulator: the check does not complete. The replay reports exactly 'Check timed out after 60.0s' — the check is cut at its budget with no assertion having fired, so the emulator accepted the calls but the asserted end state (DescribeAutoScalingGroups listing exactly one instance with LifecycleState InService after ResumeProcesses) is never observed inside the time the suite allows. Hypothesis: the group's reconciliation does not restart when the Launch process is resumed (so the recorded DesiredCapacity=1 is never filled and the poll for one InService instance spins to the budget); an alternative reading is that one of the polled DescribeAutoScalingGroups calls does not return promptly. Either way the emulator serves the sequence and the scenario never settles.
Why: a client that suspends Launch for maintenance, moves DesiredCapacity, then resumes Launch never regains capacity — botocore waiters and any poll for InService members hang until their own deadline, and scale-out automation built on suspend/resume windows deadlocks instead of completing.
Fix: the Auto Scaling group reconciliation component — the process-suspension gate that withholds launches and the launch path that must be re-entered on ResumeProcesses, plus the group-instance state (Pending -> InService transition) that DescribeAutoScalingGroups projects.
Traces: autoscaling.suspended-launch-process-blocks-launch.

Related but NON-COVERING open issues (they own other clusters, not this check): 20261115T223640Z-autoscaling-auto-scaling-standby-suspend-process-and-group-lifecycle.md (wire causes in SuspendProcesses' process-name table and refusal text), 20261118T161732Z-autoscaling-reconciliation-state-diverges-a-suspended-replaceunhealthy-still.md and 20261118T095322Z-autoscaling-ec2-an-auto-scaling-group-with-replaceunhealthy-suspended-never.md (the ReplaceUnhealthy process), 20261115T003554Z-autoscaling-ec2-elbv2-auto-scaling-a-group-with-addtoloadbalancer-suspended-still.md (AddToLoadBalancer).

## Evidence
Behavioral check autoscaling.suspended-launch-process-blocks-launch [mismatch]: 'Check timed out after 60.0s' — no AssertionError was raised, i.e. the check never reached a verdict.

The check's sequence: CreateAutoScalingGroup(MinSize=0, MaxSize=2, DesiredCapacity=0, LaunchTemplate, VPCZoneIdentifier, HealthCheckGracePeriod=0) -> SuspendProcesses(ScalingProcesses=['Launch']) -> DescribeAutoScalingGroups asserts 'Launch' in SuspendedProcesses -> SetDesiredCapacity(DesiredCapacity=1) -> DescribeAutoScalingGroups asserts DesiredCapacity == 1 -> holds and asserts NO instance appears while Launch is suspended -> ResumeProcesses(ScalingProcesses=['Launch']) -> DescribeAutoScalingGroups asserts 'Launch' no longer in SuspendedProcesses -> polls DescribeAutoScalingGroups for exactly one instance with LifecycleState 'InService'.

Expected (AWS): every call 200/Unit, DesiredCapacity recorded at 1 with zero instances while Launch is suspended, and after ResumeProcesses one group member reaching LifecycleState 'InService'.
Actual (emulator): the run is terminated at 60.0s without the final InService convergence ever being observed and without any intermediate assertion failing.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay behavioral check autoscaling.suspended-launch-process-blocks-launch (suite simparity-behavioral-aws-v1).
3. Or by hand against the emulator: create a launch template; CreateAutoScalingGroup with MinSize=0, MaxSize=2, DesiredCapacity=0 and two subnets; SuspendProcesses(AutoScalingGroupName=<group>, ScalingProcesses=['Launch']); SetDesiredCapacity(AutoScalingGroupName=<group>, DesiredCapacity=1); confirm DescribeAutoScalingGroups reports DesiredCapacity 1 and an empty Instances list; ResumeProcesses(AutoScalingGroupName=<group>, ScalingProcesses=['Launch']).
4. Poll DescribeAutoScalingGroups: AWS fills the group with one instance that reaches LifecycleState 'InService'; observe that against the emulator the group stays at zero instances (or the poll never completes), so the check exhausts its budget.
