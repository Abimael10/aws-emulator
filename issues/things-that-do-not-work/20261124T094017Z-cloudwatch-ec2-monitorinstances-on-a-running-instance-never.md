Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not
Issue-Key: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: 6879be19126f4c091ca0499123a99be6b0ef84e7

# cloudwatch__ec2: MonitorInstances on a running instance never shortens AWS/EC2 CPUUtilization to 1-minute periods, so the detailed-vs-basic twin comparison reaches no verdict

> _Note: this issue was identified against the code base at commit `6879be19126f4c091ca0499123a99be6b0ef84e7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
MonitorInstances

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: switching an instance to detailed monitoring with MonitorInstances does not change the cadence at which AWS/EC2 CPUUtilization is ingested for that instance, so no datapoint off the 5-minute grid ever appears and the twin comparison against an instance left on basic monitoring never completes.
AWS: Smithy: MonitorInstances (com.amazonaws.ec2#AmazonEC2) declares errors: [] and the model says nothing about metric cadence, so this is recorded behavior only, backed by AWS's documented rule (manage-detailed-monitoring: "Basic monitoring | Status check metrics are available in 1-minute periods. All other metrics are available in 5-minute periods." / "Detailed monitoring | You can get metrics in 1-minute periods"). The recording requires: two instances launched by one RunInstances with Monitoring omitted both report Monitoring.State 'disabled' and publish CPUUtilization (Unit 'Percent', dimension set exactly {InstanceId}) only on timestamps where epoch-seconds % 300 == 0; after MonitorInstances moves one of them to Monitoring.State 'enabled', that instance publishes CPUUtilization on 1-minute timestamps — i.e. off the 5-minute grid — within about two publication intervals, contributing ~5 datapoints to a 300 s window while the untouched twin contributes ~1 in the same window. Cadence is decided at ingest, so re-bucketing on read cannot satisfy it.
Emulator: the check never observes the divergence and is cut at the harness budget — `Error: Check timed out after 600.0s`. Hypothesis: AWS/EC2 CPUUtilization is emitted on a fixed schedule that ignores the instance's Monitoring.State, so the detailed twin and the basic twin remain indistinguishable (a sibling check in this service gets past its basic first-datapoint poll, so 5-minute publication itself exists).
Why: a client cannot tell detailed monitoring from basic monitoring — paying for MonitorInstances buys no finer resolution, an alarm written with Period=60 over AWS/EC2 CPUUtilization sits in INSUFFICIENT_DATA forever, and autoscaling or dashboards built on 1-minute EC2 resolution see nothing.
Fix: the EC2 metric-emission path that feeds AWS/EC2 into CloudWatch — make the publication period follow the instance's Monitoring.State (60 s while 'enabled', 300 s while 'disabled', switching when MonitorInstances/UnmonitorInstances settle) rather than a fixed cadence.
Traces: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not. Note: open issue 20261116T091224Z-cloudwatch-ec2-2-net-new-causes-monitorinstances-on.md describes the same emission gap under its Cause 2, but its replay keys and trace ids cover only metrics-ec2-ecs-stopped-instance-stops-publishing-but-keeps-its-history-and-aggregates, unmonitor-instances-restores-five-minute-cpu-publication-after-monitor-instances and the terminated-instance refusal — this check is not carried by it, so it is filed here and should be re-replayed alongside that issue's fix.

## Evidence
cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not, verdict [mismatch]: "Error: Check timed out after 600.0s" — the scenario is killed at the harness budget with no verdict on the cadence claim.
What the check asserts, in order: (1) both twins from one RunInstances(MinCount=2, MaxCount=2, Monitoring omitted) report Monitoring.State 'disabled'; (2) both publish at least one AWS/EC2 CPUUtilization datapoint (the positive control) and every basic datapoint satisfies int(Timestamp.timestamp()) % 300 == 0, with Unit 'Percent' and dimension set exactly {InstanceId}; (3) after MonitorInstances on one twin, that instance publishes at least one datapoint with int(Timestamp.timestamp()) % 300 != 0 within 420 s, and over one 300 s comparison span the detailed twin contributes ~5 datapoints against the basic twin's ~1.
Expected: an off-5-minute-grid CPUUtilization datapoint for the detailed twin and none for the basic twin. Actual: no off-grid datapoint is ever observed; the run ends on the budget. AMI id, instance ids and timestamps are per-run values, not part of the requirement.

## Steps to Reproduce
1. Resolve /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 via ssm GetParameter and ec2 RunInstances(ImageId=<that>, InstanceType='t3.micro', MinCount=2, MaxCount=2) with Monitoring omitted; wait for both to reach 'running' and confirm DescribeInstances reports Monitoring.State 'disabled' for both.
2. Poll cloudwatch GetMetricStatistics for AWS/EC2 CPUUtilization, Dimensions=[{InstanceId: <id>}], Period=60 for each instance until each has a datapoint; verify every Timestamp satisfies epoch-seconds % 300 == 0 and Unit is 'Percent'.
3. ec2 MonitorInstances(InstanceIds=[<first id>]); wait for Monitoring.State 'enabled'.
4. Keep polling the same metric for that instance: within ~420 s a datapoint whose epoch-seconds % 300 != 0 must appear, and over a 300 s window that instance must carry ~5 datapoints while the untouched twin still carries ~1. Re-replay cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not to confirm.
