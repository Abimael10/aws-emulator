Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudwatch__ec2.alarm-action-executes-asg-scaling-policy autoscaling__cloudwatch__ec2.suspended-alarm-notification-process-blocks-scaling-policy autoscaling__cloudwatch__ec2.suspending-alarm-notification-blocks-the-policy-driven-scale-out
Issue-Key: autoscaling__cloudwatch__ec2.alarm-action-executes-asg-scaling-policy|_poll_until#14|AssertionError|raise AssertionError(
Issue-Key: autoscaling__cloudwatch__ec2.suspended-alarm-notification-process-blocks-scaling-policy|_poll_until#15|AssertionError|raise AssertionError(
Issue-Key: autoscaling__cloudwatch__ec2.suspending-alarm-notification-blocks-the-policy-driven-scale-out|_poll#22|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s; last={last!r}")
Issue-Commit: a6c2e348e539d87c7f8cd286303d9f9234b5e1b8

# autoscaling__cloudwatch__ec2: 2 net-new causes — an alarm naming a scaling-policy ARN never executes the policy (DesiredCapacity frozen) and CloudWatch records no Action history item for it

> _Note: this issue was identified against the code base at commit `a6c2e348e539d87c7f8cd286303d9f9234b5e1b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
Two net-new causes on the CloudWatch -> Auto Scaling alarm-action hand-off, exercised here by three checks whose replay keys no open issue carries. The same boundary is described, for OTHER check ids, by open issue 20261111T064629Z-autoscaling-cloudwatch-ec2-a-cloudwatch-alarm-whose-alarmaction-is-an-auto.md; that issue does not carry the replay keys below (an ExactCapacity SimpleScaling policy, a two-group same-alarm differential, and a ChangeInCapacity policy under a suspended AlarmNotification process), so they are filed here.

Cause 1 of 2 — a CloudWatch alarm whose AlarmActions entry is an Auto Scaling PolicyARN never moves the group's capacity.
AWS: Smithy (autoscaling): PutScalingPolicy accepts PolicyType SimpleScaling with AdjustmentType ExactCapacity or ChangeInCapacity plus ScalingAdjustment and Cooldown, and returns PolicyARN (output member PolicyARN, ResourceName, max length 1600); ExecutePolicy is the operation that applies that adjustment to the group. AWS documents (https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/AlarmThatSendsEmail.html, https://docs.aws.amazon.com/autoscaling/ec2/userguide/as-scaling-simple-step.html) that on a transition to ALARM the alarm invokes each configured action, and for a scaling-policy action Auto Scaling applies the policy: an ExactCapacity policy sets DesiredCapacity to exactly ScalingAdjustment, a ChangeInCapacity policy adds ScalingAdjustment to it, MinSize/MaxSize are left alone, and a scaling activity naming the policy is recorded. Recorded behavior: with Launch suspended so no instance is provisioned, DesiredCapacity moves from 0 to the policy's exact capacity within minutes of the alarm reaching ALARM.
Emulator: the alarm does reach ALARM from the genuinely published breaching datapoint, but DesiredCapacity never changes. In alarm-action-executes-asg-scaling-policy the group stays at DesiredCapacity 0 (MinSize 0, MaxSize 4, SuspendedProcesses [Launch]) for the full 300 s budget against an ExactCapacity=2 policy. In suspended-alarm-notification-process-blocks-scaling-policy the UNSUSPENDED twin group, driven by the same alarm through its own policy, also stays at 0 — so the suspended/unsuspended differential the check measures cannot be observed at all, because neither side ever scales.
Why: every alarm-driven scaling path is dead. A client that provisions capacity through simple/step/target-tracking policies gets a group that never scales out under load, while the alarms it inspects report the breach was detected; and because the unsuspended control never scales either, SuspendProcesses(['AlarmNotification']) — modelled by Smithy SuspendProcesses with ScalingProcesses — is indistinguishable from the default, so a suppression bug and a working suppression look identical.
Fix: the Auto Scaling scaling-policy execution component (the ExecutePolicy path in the autoscaling crate) plus the alarm-action dispatch that must reach it: consuming an alarm action for a PolicyARN must apply the policy's AdjustmentType/ScalingAdjustment to DesiredCapacity, honour Cooldown, record a scaling activity naming the policy, and skip the group when AlarmNotification is in its SuspendedProcesses.

Cause 2 of 2 — no DescribeAlarmHistory Action item is written for a scaling-policy action, so the invocation is invisible.
AWS: CloudWatch exposes HistoryItemType=Action items in DescribeAlarmHistory; AWS documents that an alarm transition writes an Action item naming the invoked action ARN whose HistoryData carries the action state once the invocation was accepted. The dispatch is CloudWatch's and happens even when the Auto Scaling side declines the action (a suspended AlarmNotification process is declined by Auto Scaling, not by CloudWatch). Recorded behavior: after the breach drives the alarm to ALARM, the alarm's history contains an Action item naming the policy ARN.
Emulator: the Action-history list for the alarm stays empty for the full 300 s budget (last=[]) even though the alarm is in ALARM, so there is no record that any action was ever invoked.
Why: a client cannot tell whether its alarm ever acted. Operators and automation audit Action history to confirm an alarm fired; here every alarm looks evaluated but inert, and a broken hand-off is indistinguishable from a healthy one.
Fix: the CloudWatch alarm evaluation/action component — on each state transition dispatch every AlarmAction/OKAction/InsufficientDataAction and append the matching Action history item (with its action state) to the alarm's history, for scaling-policy ARNs as for any other target.

Traces: autoscaling__cloudwatch__ec2.alarm-action-executes-asg-scaling-policy and autoscaling__cloudwatch__ec2.suspended-alarm-notification-process-blocks-scaling-policy (Cause 1); autoscaling__cloudwatch__ec2.suspending-alarm-notification-blocks-the-policy-driven-scale-out (Cause 2).

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (160/441 checks passed; the emulator is up and answering).

Cause 1 — autoscaling__cloudwatch__ec2.alarm-action-executes-asg-scaling-policy [mismatch]:
  AssertionError: alarm <alarm> executes scaling policy <policy> and raises <group> DesiredCapacity to 2 not met within 300s. Last result: {... 'MinSize': 0, 'MaxSize': 4, 'DesiredCapacity': 0, 'Instances': [], 'SuspendedProcesses': [{'ProcessName': 'Launch', ...}] ...}
  Expected: DesiredCapacity == 2 (the ExactCapacity policy's ScalingAdjustment) with MinSize/MaxSize unchanged at 0/4 once the alarm is in ALARM.
  Actual: DesiredCapacity stayed 0 for 300 s. (Alarm, policy and group names are per-run values.)

Cause 1 — autoscaling__cloudwatch__ec2.suspended-alarm-notification-process-blocks-scaling-policy [mismatch]:
  AssertionError: the unsuspended twin <group> scales to DesiredCapacity 2 on <alarm> not met within 300s. Last result: 0
  Expected: the twin with nothing suspended reaches DesiredCapacity 2 from the shared alarm while the AlarmNotification-suspended subject stays at 0.
  Actual: the unsuspended twin also stayed at 0, so the differential is unobservable.

Cause 2 — autoscaling__cloudwatch__ec2.suspending-alarm-notification-blocks-the-policy-driven-scale-out [mismatch]:
  AssertionError: CloudWatch recorded invoking <policy> while the process was suspended not met within 300s; last=[]
  Expected: DescribeAlarmHistory(HistoryItemType=Action) lists an item naming the policy ARN after the alarm transitioned to ALARM, even though Auto Scaling declines the action for the suspended group.
  Actual: the Action-history list stayed empty for 300 s.

## Steps to Reproduce
Cause 1:
1. Create an Auto Scaling group from a launch template with MinSize=0, MaxSize=4, DesiredCapacity=0 and SuspendProcesses(['Launch']) so capacity changes are recorded without launching instances.
2. autoscaling PutScalingPolicy(PolicyType=SimpleScaling, AdjustmentType=ExactCapacity, ScalingAdjustment=2, Cooldown=0); keep the returned PolicyARN.
3. cloudwatch PutMetricAlarm on a custom metric with AlarmActions=[PolicyARN], ActionsEnabled=true, Period=60, EvaluationPeriods=1.
4. PutMetricData a breaching datapoint until DescribeAlarms reports ALARM (this already works).
5. Poll DescribeAutoScalingGroups: AWS reports DesiredCapacity 2 with MinSize/MaxSize unchanged; the emulator leaves DesiredCapacity 0 indefinitely.
6. Repeat with two groups and two policies driven by ONE alarm, suspending AlarmNotification on only one group: AWS scales the unsuspended group only; the emulator scales neither.

Cause 2:
1. Steps 1-4 above (a ChangeInCapacity policy works equally).
2. autoscaling SuspendProcesses(AutoScalingGroupName, ScalingProcesses=['AlarmNotification']).
3. cloudwatch DescribeAlarmHistory(AlarmName, HistoryItemType=Action): AWS lists an item naming the PolicyARN for the ALARM transition; the emulator lists none.
