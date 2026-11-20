Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudwatch__ec2.weighted-capacity-group-metrics-diverge-from-instance-counts
Issue-Key: autoscaling__cloudwatch__ec2.weighted-capacity-group-metrics-diverge-from-instance-counts|wait#20|Timeout|self._event.wait(delay)
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# autoscaling: a WeightedCapacity group never reaches the state where AWS/AutoScaling capacity metrics and instance metrics disagree by the instance weight — the check exhausts its whole 600s budget

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EnableMetricsCollection

## Description
Cause 1 of 1: with a weighted instance type (WeightedCapacity=2, DesiredCapacity=4 weighted units => 2 instances), the group-metric family never settles into the values AWS reports, and the check never completes.
AWS: per the EC2 Auto Scaling metrics documentation, CAPACITY metrics (GroupDesiredCapacity, GroupInServiceCapacity, GroupTotalCapacity, GroupPendingCapacity) count WEIGHTED units while INSTANCE metrics (GroupInServiceInstances, GroupTotalInstances) count instances, all with Unit 'None'; at a single 1-minute timestamp GroupInServiceCapacity = WeightedCapacity x GroupInServiceInstances, and ListMetrics indexes exactly the subset named in EnableMetricsCollection and nothing else. Recorded behavior only (the per-metric semantics are not in the Smithy API model).
Emulator: the check does not finish within its 600s budget — it never observes a common timestamp at which the weighted capacity series and the instance series carry the expected pair of values. Hypothesis: either the weighted instances never reach InService, or the AWS/AutoScaling capacity series is published in instance counts (or not published at all) after EnableMetricsCollection with Granularity=1Minute, so the polling loop can never satisfy its predicate.
Why: a client that sizes a weighted group from CloudWatch (capacity-based target tracking, dashboards, scaling math) reads capacity and instance counts that do not stand in the weight ratio AWS guarantees, or reads nothing at all, and any automation that waits for those datapoints hangs rather than failing fast. This cause is adjacent to but not the same as the open issue 20261116T231829Z-autoscaling-cloudwatch-ec2-the-enabled-subset-and-standby.md (enabled-subset/standby values), which does not carry this check's replay key.
Fix: the Auto Scaling group-metric publication component (the periodic AWS/AutoScaling emitter that derives the capacity vs. instance families from the group's weighted instances) and the InService state progression that feeds it.
Traces: autoscaling__cloudwatch__ec2.weighted-capacity-group-metrics-diverge-from-instance-counts.

## Evidence
Expected: after EnableMetricsCollection(Metrics=[GroupDesiredCapacity, GroupInServiceCapacity, GroupInServiceInstances, GroupPendingCapacity, GroupTotalCapacity, GroupTotalInstances], Granularity='1Minute') on a group whose instances each carry WeightedCapacity=2 and whose DesiredCapacity is 4, one CloudWatch minute bucket carries GroupDesiredCapacity=4, GroupInServiceInstances=2, GroupInServiceCapacity=4, GroupTotalInstances=2, GroupTotalCapacity=4, GroupPendingCapacity=0, every one with Unit 'None', and ListMetrics indexes exactly those six names.
Actual: 'Error: Check timed out after 600.0s' — the replay never reached the comparison; no satisfying bucket was ever observed within the budget.

## Steps to Reproduce
1. Create a VPC/subnet, an AL2023 launch template and an ASG with MinSize=0, MaxSize=4, DesiredCapacity=4 whose instance type carries WeightedCapacity=2 (so 2 instances cover 4 weighted units); wait for 2 InService instances.
2. EnableMetricsCollection(AutoScalingGroupName, Metrics=[the six names above], Granularity='1Minute').
3. Poll GetMetricStatistics/GetMetricData on namespace AWS/AutoScaling for those six metrics with dimension AutoScalingGroupName and look for one shared 60s bucket with capacity = 2 x instances and Unit 'None'; also ListMetrics and confirm no metric outside the enabled subset is indexed.
