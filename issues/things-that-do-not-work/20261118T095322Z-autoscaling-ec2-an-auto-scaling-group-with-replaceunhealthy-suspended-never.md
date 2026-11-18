Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended
Issue-Key: autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended|_poll#31|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# An Auto Scaling group with ReplaceUnhealthy suspended never lists its EC2-terminated member as InService/Unhealthy

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAutoScalingGroups

## Description
Cause 1 of 1: when the only member of an Auto Scaling group whose ReplaceUnhealthy process is suspended is terminated out-of-band through EC2 TerminateInstances, the group never reports that member as LifecycleState 'InService' with HealthStatus 'Unhealthy'.

AWS: the EC2 health check marks a terminated member Unhealthy independently of the suspended processes; suspending ReplaceUnhealthy only withholds the replacement. The member therefore stays listed by DescribeAutoScalingGroups with LifecycleState 'InService' and HealthStatus 'Unhealthy', DesiredCapacity stays 1, and no new scaling activity is recorded. Smithy: autoscaling DescribeAutoScalingGroups -> Instances[].LifecycleState / HealthStatus; SuspendProcesses documents ReplaceUnhealthy as the process that performs the replacement, not the health check. Recorded behaviour only for the exact timing.

Emulator: within the check's 300 s poll - taken AFTER the unsuspended control group had already completed its own replacement, so the health check had demonstrably had time to run - the suspended group never presents [(instance, 'InService', 'Unhealthy')]. The terminated member is either dropped from the membership list or left Healthy, so the state the scenario waits on is never reached and the check ends 'the run budget ended first; not graded'.

Why: a client cannot observe that suspending ReplaceUnhealthy withholds only the replacement. Health-check state of a group's members is unreadable after an out-of-band terminate, so dashboards and operators that reconcile group membership against EC2 state see a group that silently disagrees with EC2.

Fix: the autoscaling group health-check/reconciliation state - the EC2 health check must mark a member whose instance is terminated Unhealthy and keep it listed InService while ReplaceUnhealthy is suspended, instead of removing it or leaving it Healthy.

Traces: autoscaling__ec2.xsvc-autoscaling-2-ec2-terminated-member-is-replaced-at-unchanged-desired-unless-replace-unhealthy-is-suspended. Note the check reports this leg as 'not graded', so the grounded fact is the non-convergence of the membership view, not a single asserted value.

## Evidence
Expected: DescribeAutoScalingGroups on the suspended group returns exactly one Instances entry, the pre-existing instance id, with LifecycleState 'InService' and HealthStatus 'Unhealthy'; DesiredCapacity stays 1 and no activity beyond the initial launch is recorded.

Actual (replay): TimeoutError: the suspended subject listing only i-<id> as InService/Unhealthy (no replacement) after the control was replaced: the run budget ended first; not graded. The control group in the same run did replace its EC2-terminated instance, so the poll outlived a completed replacement elsewhere.

## Steps to Reproduce
1. Create two Auto Scaling groups (MinSize 0, DesiredCapacity 1, MaxSize 2) from a launch template; let each reach one InService instance.
2. SuspendProcesses(ScalingProcesses=['ReplaceUnhealthy']) on the subject group only.
3. TerminateInstances on the single member of each group and wait until EC2 reports both 'terminated'.
4. Poll DescribeAutoScalingGroups: the control replaces its member; the subject never lists its terminated member as InService/Unhealthy.
