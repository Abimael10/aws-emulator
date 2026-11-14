Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.suspended-launch-process-blocks-launch
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.suspended-launch-process-blocks-launch|_assert_stays_false#19|Timeout|time.sleep(interval_s)
Issue-Commit: a6c2e348e539d87c7f8cd286303d9f9234b5e1b8

# autoscaling: the suspended-Launch capacity-intent scenario never converges — suspended-launch-process-blocks-launch is cut off at its 60 s cap with no verdict

> _Note: this issue was identified against the code base at commit `a6c2e348e539d87c7f8cd286303d9f9234b5e1b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SuspendProcesses

## Description
One net-new cause, filed because the check is in the failing list and no open issue carries its replay key.

Cause 1 of 1: the suspended-Launch capacity-intent scenario makes no progress against the emulator and is killed at the harness cap, so it never reaches a verdict.
AWS: recorded behavior only. Smithy models SuspendProcesses(AutoScalingGroupName, ScalingProcesses) and SetDesiredCapacity(AutoScalingGroupName, DesiredCapacity); AWS documents (https://docs.aws.amazon.com/autoscaling/ec2/userguide/as-suspend-resume-processes.html) that while the Launch process is suspended a capacity increase is RECORDED — DescribeAutoScalingGroups reports the new DesiredCapacity — but no instance is created, and the recorded behavior the check encodes is exactly that: the group converges quickly to DesiredCapacity=n with Instances still empty, and each polled read answers promptly.
Emulator: the check does not finish: every step up to the first convergence poll either hangs or never satisfies its predicate, and the harness terminates the run with 'Check timed out after 60.0s'. No emulator response is captured for the step that stalls, so the scenario produces no observable outcome at all. Nothing in the failure indicates a missing runner tool, an egress or name-resolution problem, or a live-account value — the preceding calls in the same suite (CreateLaunchTemplate, CreateAutoScalingGroup, SuspendProcesses, DescribeAutoScalingGroups) are served by this emulator.
Why: the whole 'suspended Launch records intent without launching' contract is unverifiable. A client cannot establish that suspending Launch is safe, and an operator cannot distinguish a group that correctly withholds launches from one that is simply not reconciling, since the group never settles into a readable steady state within the time a non-live service should need.
Fix: the Auto Scaling group reconciliation/state component in the autoscaling crate (the loop behind SuspendProcesses + SetDesiredCapacity/UpdateAutoScalingGroup and the DescribeAutoScalingGroups read it feeds) — a capacity change on a group with Launch suspended must settle immediately to the new DesiredCapacity with an empty Instances list and be observable on the next describe, instead of leaving the scenario spinning. A fixer should re-replay the check below to see which step stalls.

Traces: autoscaling.suspended-launch-process-blocks-launch.

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (160/441 checks passed; the emulator is up and answering).

autoscaling.suspended-launch-process-blocks-launch [mismatch]:
  Error: Check timed out after 60.0s
The check's own polls allow up to 120 s per condition, so the harness cap is hit while a condition is still unmet; no assertion fired and no emulator response was captured for the stalling step. Expected: with ScalingProcesses=['Launch'] suspended, raising DesiredCapacity reads back the new DesiredCapacity on DescribeAutoScalingGroups with Instances == [] promptly. Actual: the scenario never converged and produced no verdict.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay autoscaling.suspended-launch-process-blocks-launch; it is killed at 60 s.
2. By hand: create a launch template (Amazon Linux 2023 AMI, t3.micro) and an Auto Scaling group over two subnets of the shared network.
3. autoscaling SuspendProcesses(AutoScalingGroupName, ScalingProcesses=['Launch']) and confirm SuspendedProcesses contains Launch.
4. Raise DesiredCapacity (SetDesiredCapacity or UpdateAutoScalingGroup) and poll DescribeAutoScalingGroups: AWS reports the new DesiredCapacity with Instances == [] within seconds and the read answers promptly; against the emulator the scenario never settles inside the check's budget.
