Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not
Issue-Key: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: 2659ebd94a27d67ffc464ae7266dc41ad5b86799

# cloudwatch__ec2: MonitorInstances does not switch AWS/EC2 CPUUtilization to 1-minute publication, so the detailed-vs-basic cadence comparison never reaches a verdict

> _Note: this issue was identified against the code base at commit `2659ebd94a27d67ffc464ae7266dc41ad5b86799`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
MonitorInstances

## Description
One net-new root cause.

Cause 1 of 1: an instance moved to Monitoring.State 'enabled' by MonitorInstances never begins publishing AWS/EC2 CPUUtilization on 1-minute periods, so a detailed-monitoring instance is indistinguishable from a basic-monitoring twin and the check that compares their cadence is cut by the harness budget before any assertion is graded.
Failing check id: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not.

AWS: Smithy: MonitorInstances (com.amazonaws.ec2#AmazonEC2) input {InstanceIds (required), DryRun}, output {InstanceMonitorings: list of InstanceMonitoring}, errors: [] — the model declares no errors and says nothing about metric cadence, so the cadence itself is recorded behavior only. The recording requires: both twins launched by one RunInstances call with Monitoring omitted report Monitoring.State 'disabled'; both publish AWS/EC2 CPUUtilization under the dimension set exactly {InstanceId} with Unit 'Percent', bucketed ON the 5-minute wall-clock grid (epoch second % 300 == 0); ListMetrics indexes the per-instance ('InstanceId',) AWS/EC2 series; after MonitorInstances names ONE of the two, that instance's Monitoring.State walks 'pending' -> 'enabled' while the twin stays 'disabled'; and from then on the enabled instance publishes CPUUtilization on minutes that are NOT on the 5-minute grid, so that over one identical 300-second span the enabled instance yields at least 3 datapoints and the twin at most 1. Cadence is decided at ingest: a Period=60 read cannot invent buckets that were never stored.

Emulator: the check never completes. It is cut at the harness budget with `Error: Check timed out after 600.0s` — no response is produced for the discriminating observation, so the emulator never serves an off-5-minute-grid CPUUtilization datapoint for the instance MonitorInstances enabled (and, upstream of that, never settles the basic-monitoring positive control plus the switch within the budget). Hypothesis: AWS/EC2 CPUUtilization is emitted on a fixed 5-minute schedule that ignores the instance's Monitoring.State, so MonitorInstances changes the reported state but nothing in the metric-emission path.

Why: a client that pays for detailed monitoring gets no finer resolution. Alarms and dashboards built on Period=60 for AWS/EC2 CPUUtilization see INSUFFICIENT_DATA or stale 5-minute buckets forever, autoscaling and anomaly detection configured against 1-minute data never fire, and code that distinguishes detailed from basic monitoring by observing the stored bucket cadence cannot tell the two apart.

Fix: the EC2 metric-emission path that feeds the AWS/EC2 namespace into CloudWatch, plus the MonitorInstances/UnmonitorInstances handlers' state transition that must drive it — the publication period must follow the instance's Monitoring.State (60 s while 'enabled', 300 s while 'disabled'), switching when MonitorInstances/UnmonitorInstances settle, with Unit 'Percent' and the dimension set {InstanceId}.

Related open issue (does NOT carry this check's replay key, so it does not cover it): 20261116T091224Z-cloudwatch-ec2-2-net-new-causes-monitorinstances-on.md — its Cause 2 describes the same 1-minute-cadence gap for the sibling checks metrics-ec2-ecs-stopped-instance-stops-publishing-but-keeps-its-history-and-aggregates and unmonitor-instances-restores-five-minute-cpu-publication-after-monitor-instances. This check (detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not) is not listed in that issue's Issue-Trace-Ids and was not reported as owned, so it is filed here; a fixer closing that issue should re-replay this check too.

Traces: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not (suite report, section `### cloudwatch__ec2`).

## Evidence
Suite report, `### cloudwatch__ec2`:
  [mismatch] cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not
  Error: Check timed out after 600.0s

Expected (recorded AWS behavior): after ec2:MonitorInstances names exactly one of two same-age t3.micro instances, DescribeInstances reports Monitoring.State 'pending' then 'enabled' for that instance and 'disabled' for the twin; cloudwatch:GetMetricStatistics(Namespace='AWS/EC2', MetricName='CPUUtilization', Dimensions=[{InstanceId}], Period=60, Statistics=['Average','SampleCount']) then returns, for the enabled instance, at least one datapoint whose timestamp is NOT a multiple of 300 seconds, and over one identical 300 s span at least 3 datapoints for the enabled instance versus at most 1 for the twin; every datapoint carries Unit 'Percent'; before the switch both instances' datapoints are only on the 300 s grid; cloudwatch:ListMetrics indexes the ('InstanceId',) dimension set for the AWS/EC2 series.

Actual: the check produces no verdict at all — it is terminated by the harness after 600.0 s, which means the emulator did not serve the awaited off-5-minute-grid CPUUtilization datapoint (nor settle the preceding basic-monitoring positive control and the Monitoring.State switch) inside that budget. No off-grid bucket is ever observed, so the detailed and basic twins remain indistinguishable in the stored series.

Note on per-run values: instance ids, the resolved AMI id, timestamps and the exact elapsed seconds differ every run and are not part of the requirement; the requirement is the cadence (60 s vs 300 s bucketing), the Unit 'Percent', and the {InstanceId} dimension set.

## Steps to Reproduce
1. Replay the behavioral suite and re-run the check cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not.

Manually:
1. ssm:GetParameter(Name='/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64') to resolve an AMI id.
2. ec2:RunInstances(ImageId=<that ami>, InstanceType='t3.micro', MinCount=2, MaxCount=2, SubnetId=<a subnet of the shared VPC>) with Monitoring omitted; wait for both instances to reach 'running' and confirm DescribeInstances reports Monitoring.State 'disabled' for both.
3. Poll cloudwatch:GetMetricStatistics(Namespace='AWS/EC2', MetricName='CPUUtilization', Dimensions=[{'Name':'InstanceId','Value':<id>}], Period=60, Statistics=['Average','SampleCount']) for both instances until each has at least one datapoint; assert every datapoint timestamp satisfies int(ts) % 300 == 0 and Unit == 'Percent'.
4. cloudwatch:ListMetrics(Namespace='AWS/EC2', Dimensions=[{'Name':'InstanceId','Value':<first id>}]) and assert the ('InstanceId',) dimension set is present.
5. ec2:MonitorInstances(InstanceIds=[<first id>]); assert the response reports Monitoring.State in {'pending','enabled'} for it, that the twin still reports 'disabled', and poll DescribeInstances until the first instance reports 'enabled'.
6. Poll the Period=60 read for the enabled instance for a datapoint whose timestamp is after the switch and NOT a multiple of 300 — AWS produces one within ~7 one-minute intervals; the emulator never does.
7. After 300 s of wall clock since the switch, read both instances over the SAME 300 s span: the enabled instance must return >= 3 datapoints, the twin <= 1.
8. ec2:TerminateInstances(InstanceIds=[both]) to clean up.
