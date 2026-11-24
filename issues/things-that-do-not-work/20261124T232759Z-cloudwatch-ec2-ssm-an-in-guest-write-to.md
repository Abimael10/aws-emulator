Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ssm.obs-ec2-2-ebs-consumed-iops-published-for-io2-and-withheld-from-a-gp3-written-in-the-same-instant cloudwatch__ec2__ssm.obs-ec2-2-ebs-reads-and-writes-land-on-separate-series-under-exactly-the-volume-id-dimension cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-series-follows-the-volume-across-a-move-to-a-second-instance cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-telemetry-published-only-while-attached
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-2-ebs-consumed-iops-published-for-io2-and-withheld-from-a-gp3-written-in-the-same-instant|_write_round#3|AssertionError|assert int(_marker(out, f"EBS-W-{vid}") or 0) == w_bytes, (
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-2-ebs-reads-and-writes-land-on-separate-series-under-exactly-the-volume-id-dimension|run#3|AssertionError|assert int(_marker(out, "EBS-W") or 0) == w_bytes, f"guest write count wrong: {out!r}"
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-series-follows-the-volume-across-a-move-to-a-second-instance|_guest_write#3|AssertionError|assert int(_marker(out, "EBS-DD") or 0) == mib * _MIB, (
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-telemetry-published-only-while-attached|run#3|AssertionError|assert written == _WRITE_BYTES, (
Issue-Commit: 4d64375b908e4858d980b965a76727675c50db36

# cloudwatch__ec2__ssm: an in-guest write to an attached EBS volume transfers no bytes — dd exits 0 and reports no transfer, so no AWS/EBS per-volume telemetry can be produced

> _Note: this issue was identified against the code base at commit `4d64375b908e4858d980b965a76727675c50db36`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachVolume

## Description
Cause 1 of 1: a command run through SSM Run Command on an SSM-managed node that writes to (or reads from) the block device of an attached EBS volume moves no data and reports no transfer, so every AWS/EBS per-volume metric scenario stops at its first in-guest I/O gate (failing checks: cloudwatch__ec2__ssm.obs-ec2-2-ebs-consumed-iops-published-for-io2-and-withheld-from-a-gp3-written-in-the-same-instant, cloudwatch__ec2__ssm.obs-ec2-2-ebs-reads-and-writes-land-on-separate-series-under-exactly-the-volume-id-dimension, cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-series-follows-the-volume-across-a-move-to-a-second-instance, cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-telemetry-published-only-while-attached).

AWS: recorded behavior only (no Smithy member governs in-guest device I/O; ec2 AttachVolume/DescribeVolumes report the attachment as in-use/attached, and the checks confirm the emulator already reports that, and already resolves the volume's NVMe serial). On a real instance, a volume attached with ec2 AttachVolume is presented in the guest as an NVMe block device reachable at /dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_<volume id with hyphens removed>. A write to that device (here `dd if=/dev/urandom of=<device> bs=1M count=<N> oflag=direct conv=fsync`) actually stores the bytes, exits 0, and writes coreutils' three-line transfer summary to stderr whose third line begins "<bytes> bytes (...) copied, ..." — the byte count equals bs*count. The same holds for a read with iflag=direct. Those transferred bytes are what CloudWatch then publishes under AWS/EBS on the dimension set [VolumeId] as VolumeWriteBytes / VolumeReadBytes / VolumeWriteOps / VolumeReadOps (and VolumeConsumedReadWriteOps / VolumeThroughputPercentage for io1/io2 only), at Period=60. The requirement is the general rule: bytes a guest writes to an attached volume's device are transferred, counted and reported; the specific byte counts (16/48/64 MiB), volume ids and instance ids in the recordings are per-run values filling that template.

Emulator: the device node resolves and `lsblk -dno SERIAL` returns the volume's serial (the EBS-SERIAL marker is correct in every one of the four recordings), but the `dd` that follows exits 0 while transferring nothing and emitting no transfer summary at all: the stderr capture the check greps for '^[0-9]* bytes' is empty, so the byte-count marker comes back blank. Recorded stdout, with the emulator's own exit code on the same line: 'EBS-SERIAL-vol-...ca vol000000000000000ca\nEBS-RC-vol-...ca 0\nEBS-W-vol-...ca \n...' (consumed-iops), 'EBS-SERIAL vol000000000000000ca\nEBS-W-RC 0\nEBS-W \nEBS-R-RC 0\nEBS-R \n' (read/write split), 'EBS-SERIAL vol000000000000000ce\nEBS-IID i-00000000000000001\nEBS-DD-RC 0\nEBS-DD \n' (volume move), and "dd inside the guest reported 0 bytes written to /dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_vol000000000000000ca, expected 67108864" (attached-only telemetry). Exit status 0 with an empty transfer report is self-contradictory: the guest claims the write succeeded and simultaneously reports that nothing moved. Because no bytes move, nothing is ever published to AWS/EBS either, so the downstream assertions (per-direction series, the exact [VolumeId] dimension set, io2-only metrics withheld from gp3, the series following a volume across a re-attach, telemetry stopping at detach) are all unreachable.

Why: a client cannot do EBS I/O from inside an instance at all. Any workload that writes to an attached data volume silently stores nothing while every status it can inspect (dd's exit code, the attachment state) says success — the worst failure mode, because it is invisible. All EBS observability built on it is dead: per-volume throughput/IOPS dashboards, alarms on VolumeWriteBytes/VolumeReadBytes, capacity and burst-balance work, and the correlation of guest device to volume id that monitoring agents and runbooks depend on.

Fix: the ec2 data plane's volume attachment and the guest command execution path behind SSM Run Command — a write or read issued against an attached volume's device node must transfer the bytes to that volume's backing store and the guest program must return its real exit status and stderr (so a transfer summary is emitted when the tool emits one), and the transferred bytes must feed the AWS/EBS metric publisher that CloudWatch serves on the [VolumeId] dimension. Open issue 20261115T034057Z-cloudwatch-ec2-ssm-an-attached-ebs-volume-presents.md is the neighbour here and does not cover this: it reported the serial AND the write both coming back empty, and its replay key is the EBS-SERIAL assertion. The serial half now resolves, so all four checks have advanced past that key to the write assertion, which is the cause filed here.

Traces: the four failing checks named above; each one's first in-guest I/O gate.

## Evidence
Expected (real AWS): dd of N MiB to /dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_<serial> exits 0 and its stderr carries "<N*1048576> bytes ... copied"; AWS/EBS then shows VolumeWriteBytes >= N*1048576 (and VolumeReadBytes for the read leg) on dimension set [VolumeId], Period=60.

Actual (emulator), per failing check:
- obs-ec2-2-ebs-consumed-iops-...: AssertionError: round 1: the guest did not write 16777216 bytes to vol-000000000000000ca: stdout='EBS-SERIAL-vol-000000000000000ca vol000000000000000ca\nEBS-SERIAL-vol-000000000000000cb vol000000000000000cb\nEBS-RC-vol-000000000000000ca 0\nEBS-W-vol-000000000000000ca \nEBS-RC-vol-000000000000000cb 0\nEBS-W-vol-000000000000000cb \n'
- obs-ec2-2-ebs-reads-and-writes-land-on-separate-series-...: AssertionError: guest write count wrong: 'EBS-SERIAL vol000000000000000ca\nEBS-W-RC 0\nEBS-W \nEBS-R-RC 0\nEBS-R \n'
- obs-ec2-2-ebs-write-series-follows-the-volume-...: AssertionError: dd on i-00000000000000001 did not write 25165824 bytes: stdout='EBS-SERIAL vol000000000000000ce\nEBS-IID i-00000000000000001\nEBS-DD-RC 0\nEBS-DD \n'
- obs-ec2-2-ebs-write-telemetry-published-only-while-attached: AssertionError: dd inside the guest reported 0 bytes written to /dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_vol000000000000000ca, expected 67108864

In all four, the exit-code marker (EBS-RC / EBS-W-RC / EBS-DD-RC) is "0" — success — while the byte-count marker is empty. Volume ids, instance ids and byte counts are per-run values; the requirement is that the reported count equal the requested transfer and that the bytes reach the volume.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it (the behavioral suite does this).
2. Create a VPC/subnet, an instance profile whose role has arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore, and launch a t3.micro from /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64; wait for the node to be an Online SSM managed node.
3. ec2 CreateVolume(Size=1, VolumeType='gp3') and AttachVolume(VolumeId=<vol>, InstanceId=<iid>, Device='/dev/sdf'); wait for the attachment to read in-use/attached.
4. ssm SendCommand(DocumentName='AWS-RunShellScript') on the node with:
   dev=/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_$(echo <vol> | tr -d -)
   lsblk -dno SERIAL $(readlink -f $dev)            # correct today
   dd if=/dev/urandom of=$dev bs=1M count=64 oflag=direct conv=fsync 2> /tmp/w.err; echo "RC $?"
   grep -o '^[0-9]* bytes' /tmp/w.err
5. GetCommandInvocation: Status is Success and RC is 0, but the grep output is empty — no bytes were reported and none were transferred. Expected "67108864 bytes".
6. cloudwatch GetMetricStatistics(Namespace='AWS/EBS', MetricName='VolumeWriteBytes', Dimensions=[{Name:'VolumeId',Value:<vol>}], Period=60, Statistics=['Sum']) over the surrounding minutes — no datapoint covers the write.
