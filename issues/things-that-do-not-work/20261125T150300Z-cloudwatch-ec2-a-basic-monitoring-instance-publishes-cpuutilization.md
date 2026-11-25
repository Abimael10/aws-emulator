Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not
Issue-Key: cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not|run#32|AssertionError|assert len(basic_in_span) <= 1, (
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# cloudwatch__ec2: a basic-monitoring instance publishes CPUUtilization every minute, so basic and detailed monitoring are indistinguishable

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: AWS/EC2 CPUUtilization is published at a 1-minute cadence for an instance that was never switched to detailed monitoring, so the 5-minute basic period is not honoured and MonitorInstances has no observable effect.

AWS: the EC2 metric publication cadence is documented, not modelled — Smithy (services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801) models GetMetricStatistics/GetMetricData with no statement about publication period, so this is recorded/documented behavior only. AWS documents (EC2 User Guide, manage-detailed-monitoring: "Basic monitoring | Status check metrics are available in 1-minute periods. All other metrics are available in 5-minute periods." / "Detailed monitoring | You can get metrics in 1-minute periods") that for an instance under BASIC monitoring every metric other than the status checks — CPUUtilization among them — is published once per 5-minute wall-clock bucket (epoch second % 300 == 0), and only MonitorInstances/detailed monitoring shortens that to one datapoint per minute.

Emulator: over a 300 s span, the basic-monitoring instance reports MORE THAN ONE CPUUtilization datapoint, on minute boundaries that are not multiples of 5. From the replay: "over the SAME span the basic-monitoring twin i-<id> published 2 CPUUtilization datapoints at ['...T14:35:00+00:00', '...T14:40:00+00:00']; a 300s window is exactly one basic period, so more than one datapoint means the 5-minute cadence is not being honoured". The instance id and timestamps are per-run values; the requirement is one datapoint per 300 s bucket under basic monitoring.

Why: a client cannot tell basic monitoring from detailed monitoring, and any code that sizes an alarm period, a dashboard period or a billing estimate off the published cadence is wrong: it will accept a 60 s Period for a basic instance (which AWS answers with empty datapoint sets) and will see no change after enabling detailed monitoring, so MonitorInstances appears to be a no-op.

Fix: the EC2 instance-metric emitter that publishes AWS/EC2 datapoints into the cloudwatch store — it must bucket non-status-check metrics on 300 s boundaries while an instance's monitoring state is `disabled`, and only drop to 60 s buckets once MonitorInstances has set it to `enabled`.

Traces: check cloudwatch__ec2.detailed-monitoring-shortens-the-cpu-metric-period-and-basic-does-not.

## Evidence
Expected: for an instance left under basic monitoring, GetMetricStatistics over a 300 s span returns at most ONE AWS/EC2 CPUUtilization datapoint, timestamped on a 5-minute wall-clock boundary (epoch second % 300 == 0), Unit=Percent, dimension set exactly {InstanceId}; only after MonitorInstances does the same read show per-minute datapoints.

Actual: AssertionError — "over the SAME span the basic-monitoring twin i-02a4b897830f4d4dd published 2 CPUUtilization datapoints at ['2026-11-25T14:35:00+00:00', '2026-11-25T14:40:00+00:00']; a 300s window is exactly one basic period, so more than one datapoint means the 5-minute cadence is not being honoured — this is the leg a target that re-buckets on the READ period fails". Verdict: mismatch.

## Steps to Reproduce
1. Launch two t3.micro instances from the SSM public AL2023 AMI parameter and wait for both to reach `running`, leaving monitoring at its default (basic).
2. Call MonitorInstances on exactly one of them.
3. Let ~7 minutes of metrics accumulate.
4. For each instance call GetMetricStatistics(Namespace="AWS/EC2", MetricName="CPUUtilization", Dimensions=[{InstanceId}], Period=60) over the same 300 s span.
5. Observe the basic-monitoring twin reports more than one datapoint in that span, on non-multiple-of-5 minute boundaries.
