Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not
Issue-Key: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# No AWS/EC2 CPUUtilization datapoint is ever published for a running instance, so the basic vs detailed monitoring cadence is unobservable

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: a t3.micro that reaches 'running' publishes no AWS/EC2 CPUUtilization series, so the check that distinguishes 5-minute (basic) from 1-minute (detailed, after MonitorInstances) publication never sees a first datapoint and is killed at the run budget with no verdict.

AWS: an instance with basic monitoring publishes CPUUtilization in 5-minute periods aligned to 5-minute wall-clock boundaries; after MonitorInstances the same metric is published in 1-minute periods (EC2 user guide, manage-detailed-monitoring: 'Basic monitoring | Status check metrics are available in 1-minute periods. All other metrics are available in 5-minute periods.' / 'Detailed monitoring | You can get metrics in 1-minute periods'). The series carries Unit 'Percent' and exactly the {InstanceId} dimension. Smithy: cloudwatch GetMetricStatistics(Namespace='AWS/EC2', MetricName='CPUUtilization', Dimensions=[{Name:'InstanceId'}]) returns Datapoints; ec2 MonitorInstances switches the cadence.

Emulator: GetMetricStatistics/ListMetrics report no CPUUtilization datapoint for the running instance at all, so the check cannot even establish the basic-monitoring baseline and ends 'Check timed out after 600.0s'. No status, error code or message is involved - the series is simply absent.

Why: every client that alarms, autoscales or reports on EC2 CPU reads an empty metric. Alarms on AWS/EC2 CPUUtilization stay INSUFFICIENT_DATA forever, and the documented difference between basic and detailed monitoring cannot be observed.

Fix: the CloudWatch metric-publication state for EC2 instances (the EC2 instance metric publisher feeding the cloudwatch metric store) - a running instance must publish AWS/EC2 CPUUtilization under the {InstanceId} dimension with Unit Percent on 5-minute boundaries, and on 1-minute boundaries once MonitorInstances is called.

Related open/resolved work that does NOT carry this check's replay key: open issue 20261109T183935Z-cloudwatch-ec2-2-net-new-causes-no-aws.md describes the same missing AWS/EC2 series, and the resolved issue 20261110T160612Z-cloudwatch-ec2-no-aws-ec2-cpuutilization-series-is.md claimed this exact cadence scenario; the series is still absent in this replay.

Traces: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not.

## Evidence
Expected: within the check's monitoring budget, GetMetricStatistics(Namespace 'AWS/EC2', MetricName 'CPUUtilization', Dimensions [{Name 'InstanceId', Value <id>}], Period 300) returns at least one Datapoint with Unit 'Percent' on a 5-minute boundary; after MonitorInstances, datapoints appear at minutes that are not multiples of 5.

Actual (replay): Error: Check timed out after 600.0s - no first datapoint ever appears for the running instance, so neither leg can be graded.

## Steps to Reproduce
1. RunInstances a t3.micro (AL2023 via the SSM public AMI parameter) into the run's network and wait for 'running'.
2. Poll cloudwatch GetMetricStatistics for AWS/EC2 CPUUtilization with Dimensions=[{InstanceId}] and Period 300: no Datapoints are ever returned.
3. (Unreachable) MonitorInstances on the instance and poll with Period 60 to see 1-minute publication.
