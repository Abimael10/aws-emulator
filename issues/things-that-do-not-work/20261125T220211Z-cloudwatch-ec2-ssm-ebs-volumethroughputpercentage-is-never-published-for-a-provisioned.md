Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ssm.obs-ec2-2-ebs-consumed-iops-published-for-io2-and-withheld-from-a-gp3-written-in-the-same-instant
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-2-ebs-consumed-iops-published-for-io2-and-withheld-from-a-gp3-written-in-the-same-instant|_poll#3|AssertionError|raise AssertionError(
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# EBS VolumeThroughputPercentage is never published for a Provisioned IOPS (io2) volume

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
One net-new root cause, grounded in the failing behavioral check under the suite heading `cloudwatch__ec2__ssm`.

Cause 1 of 1: an io2 volume publishes VolumeConsumedReadWriteOps for in-guest I/O but never publishes VolumeThroughputPercentage.
AWS: recorded behavior only (metric emission is not described by any Smithy model). AWS/EBS publishes the Provisioned-IOPS-only pair VolumeConsumedReadWriteOps (Unit Count) and VolumeThroughputPercentage (Unit Percent) for io1/io2 volumes, dimensioned by VolumeId, in the same one-minute buckets as the generic series (VolumeWriteBytes etc.); gp3 and other non-provisioned types publish neither. The check grades exactly that contrast: for the same instant's write, the io2 volume must show at least one bucket of BOTH provisioned-IOPS metrics while the gp3 twin shows neither.
Emulator: after an identical in-guest write to both volumes, VolumeWriteBytes appears for both (Sum 16777216 Bytes, same minute bucket) and VolumeConsumedReadWriteOps appears for the io2 volume (Sum 64 Count), but VolumeThroughputPercentage is empty for the io2 volume in every bucket for the whole 240 s round budget — the series is never published at all. (The gp3 side is correct: both provisioned-IOPS metrics are absent.)
Why: a client that alarms or dashboards on provisioned-IOPS saturation — VolumeThroughputPercentage is the canonical "how much of my provisioned throughput am I using" series — gets an alarm permanently in INSUFFICIENT_DATA and an empty graph, and cannot distinguish "not saturated" from "not measured".
Fix: the EBS metric emission for provisioned-IOPS volume types must publish the VolumeThroughputPercentage series alongside VolumeConsumedReadWriteOps into cloudwatch's AWS/EBS namespace with the VolumeId dimension (the EC2/EBS telemetry publisher, services/service-com-amazonaws-ec2-amazonec2/src). The open issue 20261124T232759Z-cloudwatch-ec2-ssm-an-in-guest-write-to.md carries the other EBS write-telemetry checks under this heading and does not carry this check or the VolumeThroughputPercentage series.
Traces: cloudwatch__ec2__ssm.obs-ec2-2-ebs-consumed-iops-published-for-io2-and-withheld-from-a-gp3-written-in-the-same-instant.

## Evidence
AssertionError: round 1: VolumeWriteBytes >= 16777216 for both volumes and the io2 volume vol-<io2> publishing ('VolumeConsumedReadWriteOps', 'VolumeThroughputPercentage') in >= 1 bucket(s) not met within 240s (check-wide deadline 600s, 252s used; last observed:
  (vol-<gp3>, 'VolumeWriteBytes'): [{Sum: 16777216.0, Unit: 'Bytes'}]
  (vol-<gp3>, 'VolumeConsumedReadWriteOps'): []
  (vol-<gp3>, 'VolumeThroughputPercentage'): []
  (vol-<io2>, 'VolumeWriteBytes'): [{Sum: 16777216.0, Unit: 'Bytes'}]
  (vol-<io2>, 'VolumeConsumedReadWriteOps'): [{Sum: 64.0, Unit: 'Count'}]
  (vol-<io2>, 'VolumeThroughputPercentage'): [])

Expected (AWS): the io2 volume has >= 1 bucket for BOTH VolumeConsumedReadWriteOps and VolumeThroughputPercentage; the gp3 volume has neither.
Actual (emulator): VolumeThroughputPercentage is never published for the io2 volume.

## Steps to Reproduce
1. Launch an SSM-managed instance; create and attach two EBS volumes to it — one io2 (provisioned IOPS) and one gp3.
2. Via SSM RunCommand, write 16 MiB to each volume in the same instant and flush.
3. Poll cloudwatch GetMetricStatistics in AWS/EBS with dimension VolumeId for VolumeWriteBytes, VolumeConsumedReadWriteOps and VolumeThroughputPercentage over the covering minutes: the io2 volume reports VolumeWriteBytes and VolumeConsumedReadWriteOps but VolumeThroughputPercentage stays empty.
