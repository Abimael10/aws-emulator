Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudwatch__ec2.group-metrics-reach-cloudwatch-only-for-the-enabled-subset autoscaling__cloudwatch__ec2.obs-autoscaling-2-standby-group-metrics-values-and-per-metric-disable
Issue-Key: autoscaling__cloudwatch__ec2.group-metrics-reach-cloudwatch-only-for-the-enabled-subset|_poll#17|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling__cloudwatch__ec2.obs-autoscaling-2-standby-group-metrics-values-and-per-metric-disable|wait#27|Timeout|self._event.wait(delay)
Issue-Commit: c037406dab1b28830e94eb95f4ccf1fc05361c78

# autoscaling__cloudwatch__ec2: the enabled-subset and standby group-metric scenarios never reach an observable step — both are killed at the 600 s budget with no AWS/AutoScaling datapoint and no converged fleet

> _Note: this issue was identified against the code base at commit `c037406dab1b28830e94eb95f4ccf1fc05361c78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EnableMetricsCollection

## Description
Cause 1 of 1: an Auto Scaling group whose group metrics have been enabled with EnableMetricsCollection produces no observable AWS/AutoScaling metric state at all, and the group never converges to the InService fleet those metrics are supposed to describe, so both end-to-end group-metric scenarios run out their 600 s budget without ever reaching the assertion they exist to make.

AWS: recorded behavior only for the delivery itself — the autoscaling Smithy model (AutoScaling_2011_01_01, awsQuery) declares EnableMetricsCollection with required AutoScalingGroupName and Granularity and models only ResourceContentionFault, and DescribeMetricCollectionTypes declares the metric-collection-type and granularity catalogues; nothing in the model describes publication. What the recordings show is: after EnableMetricsCollection(Granularity='1Minute') on a group, Auto Scaling publishes the enabled group metrics into the CloudWatch namespace AWS/AutoScaling dimensioned by AutoScalingGroupName, at 1-minute granularity, with Unit 'None'; the first datapoint reflecting a capacity change lands within about a minute of that change; the series exists for exactly the metric names that were enabled and for no others; and each metric reads the value the group's state implies (e.g. after EnterStandby with ShouldDecrementDesiredCapacity=true on one of three InService instances: GroupDesiredCapacity and GroupInServiceInstances fall by one, GroupStandbyInstances is 1, GroupTotalInstances is unchanged). Independently, a group created with a valid launch template and DesiredCapacity=N reaches N instances in LifecycleState 'InService' and DescribeAutoScalingGroups reports the decremented DesiredCapacity after EnterStandby.

Emulator: neither scenario produces any of that within 600 s. Both checks terminate with 'Check timed out after 600.0s' while polling — they never get past the setup polls that wait for the group to reach its InService fleet and for the first AWS/AutoScaling datapoint of the enabled metric, so no verdict about the enabled subset, the per-metric disable, or the standby values is ever produced. The two checks differ only in what they would have asserted (swapped single-metric subsets across two groups vs. all-metrics-enabled values after EnterStandby), so one missing capability starves both: enabled group metrics are not published to AWS/AutoScaling for the group, and/or the group never converges to an InService fleet to publish about.

Why: a client cannot observe its Auto Scaling groups at all. Dashboards, scaling runbooks and any CloudWatch alarm whose metric is an AWS/AutoScaling group metric (GroupDesiredCapacity, GroupInServiceInstances, GroupStandbyInstances, GroupTotalInstances and their *Capacity twins) stay permanently in INSUFFICIENT_DATA against the emulator, and nothing that waits on a group to reach its desired capacity ever proceeds. Because the calls return success, the failure is silent: EnableMetricsCollection is accepted and DescribeAutoScalingGroups lists the metric as enabled while no series is ever created.

Fix: the Auto Scaling group reconciler and its CloudWatch publication path in the autoscaling crate — the component that drives group members to InService/Standby and the one that, for each metric named in a group's EnabledMetrics, emits a 1-minute AWS/AutoScaling datapoint dimensioned by AutoScalingGroupName with the value the group state implies (and stops emitting exactly the series that DisableMetricsCollection removes, while leaving its enabled twins alone).

Adjacent open issues named for reference, neither of which carries these two checks' replay keys: 20261109T172356Z-autoscaling-cloudwatch-ec2-a-weighted-capacity-auto-scaling.md and 20261110T160612Z-autoscaling-cloudwatch-ec2-a-weighted-capacity-auto-scaling-cb74ea.md cover the WEIGHTED-capacity group-metric comparison; the two checks here are the unweighted enabled-subset and standby-value scenarios and are reported as net-new (not ALREADY FILED, not OWNED).

Traces: autoscaling__cloudwatch__ec2.group-metrics-reach-cloudwatch-only-for-the-enabled-subset, autoscaling__cloudwatch__ec2.obs-autoscaling-2-standby-group-metrics-values-and-per-metric-disable.

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against the emulator built from this checkout:

  [mismatch] autoscaling__cloudwatch__ec2.group-metrics-reach-cloudwatch-only-for-the-enabled-subset
    Error: Check timed out after 600.0s
    The check creates two groups from one launch template (min 0 / max 2 / desired 1), enables exactly GroupDesiredCapacity on one and exactly GroupTotalInstances on the other, waits for 1 InService instance in each, raises both to DesiredCapacity=2, then polls GetMetricStatistics/GetMetricData in namespace AWS/AutoScaling for a datapoint whose value is 2.0. It never gets past those polls.

  [mismatch] autoscaling__cloudwatch__ec2.obs-autoscaling-2-standby-group-metrics-values-and-per-metric-disable
    Error: Check timed out after 600.0s
    The check creates a group with MinSize=1/MaxSize=4/DesiredCapacity=3, waits for 3 InService instances, calls EnableMetricsCollection(Granularity='1Minute') with no Metrics (enable-all), puts one instance into Standby with ShouldDecrementDesiredCapacity=True, and then expects AWS/AutoScaling to read GroupMinSize=1, GroupMaxSize=4, GroupDesiredCapacity=2, GroupInServiceInstances=2, GroupStandbyInstances=1, GroupTotalInstances=3, GroupPendingInstances=0, GroupTerminatingInstances=0, GroupInServiceCapacity=2, that ListMetrics indexes exactly the enabled names, and that disabling GroupStandbyInstances stops only that series while GroupDesiredCapacity keeps receiving datapoints. It never reaches any of those assertions.

Both checks are reported under '### autoscaling__cloudwatch__ec2' with no ALREADY FILED and no OWNED marker. The 600 s expiry is the emulator's own non-convergence, not a runner defect: the runner completed setup calls (CreateLaunchTemplate / CreateAutoScalingGroup / EnableMetricsCollection all returned) and only the polls for emulator-produced state never satisfied.

Model grounding: services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json — EnableMetricsCollection(AutoScalingGroupName required, Granularity required, Metrics list), DisableMetricsCollection(AutoScalingGroupName required, Metrics list), DescribeMetricCollectionTypes -> {Metrics, Granularities}; none models the publication, so the delivery contract above is recorded behavior only.

## Steps to Reproduce
1. git clone the project repo; build and boot the emulator from the checkout.
2. Replay suite simparity-behavioral-aws-v1 and re-run the two checks in trace_ids.
3. Minimal repro for the publication leg: create a launch template with a valid AMI and t3.micro; create an Auto Scaling group with MinSize=0, MaxSize=2, DesiredCapacity=1 over the run's subnets; call autoscaling EnableMetricsCollection(AutoScalingGroupName=<group>, Granularity='1Minute', Metrics=['GroupDesiredCapacity']); confirm DescribeAutoScalingGroups lists that metric in EnabledMetrics; call SetDesiredCapacity(DesiredCapacity=2); then poll cloudwatch GetMetricStatistics(Namespace='AWS/AutoScaling', MetricName='GroupDesiredCapacity', Dimensions=[{AutoScalingGroupName:<group>}], Period=60, Statistics=['Average']) — AWS produces a datapoint reading 2.0 (Unit 'None') within about a minute; the emulator returns no datapoints indefinitely.
4. Minimal repro for the convergence leg: with the same group at DesiredCapacity=3 (MaxSize>=3), poll DescribeAutoScalingGroups/DescribeAutoScalingInstances for 3 members in LifecycleState 'InService' — the group never reaches them, so EnterStandby and the standby-value assertions are unreachable.
5. Also required by the second check once publication works: ListMetrics must index exactly the enabled metric names, and DisableMetricsCollection(Metrics=['GroupStandbyInstances']) must stop only that series while GroupDesiredCapacity keeps receiving datapoints.
6. Both checks must pass before closing.
