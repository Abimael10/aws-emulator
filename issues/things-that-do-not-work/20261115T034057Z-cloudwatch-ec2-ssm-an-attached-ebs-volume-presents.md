Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-series-follows-the-volume-across-a-move-to-a-second-instance
Issue-Key: cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-series-follows-the-volume-across-a-move-to-a-second-instance|_guest_write#3|AssertionError|assert _marker(out, "EBS-SERIAL") == volume_id.replace("-", ""), (
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# cloudwatch__ec2__ssm: an attached EBS volume presents no NVMe serial inside the guest and the in-guest read returns empty output, so per-volume telemetry cannot be followed across a move

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachVolume

## Description
=== Cause 1 of 1: the guest sees neither the volume's serial nor any command output for the attached device ===
AWS: recorded behavior only. A volume attached to an instance appears in the guest as an NVMe device whose by-id link `/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_<serial>` carries the volume's id with the hyphen removed as its serial, and a command run in the guest that reads that serial (and the instance id, and writes to the device) returns those values on stdout. That is how a client correlates a guest block device with the EBS volume id that CloudWatch publishes EBS metrics under.
Emulator: the in-guest read returns empty fields — stdout is `'EBS-SERIAL \nEBS-IID <instance-id>\nEBS-DD \n'`: the serial marker and the device-write marker are blank, so the device behind the by-id path does not identify the volume and the write never reports. Only the instance id is filled in.
Why: a client cannot tell which guest device is which EBS volume, so it cannot attribute in-guest I/O to a volume id, cannot verify that a volume's write telemetry follows the volume when it is detached from one instance and attached to another, and any guest-side EBS bookkeeping (udev rules, mount-by-id, monitoring agents keyed on the serial) is broken. The open issue 20261114T222555Z-cloudwatch-ec2-ssm-3-net-new-causes-a.md records neighbouring EBS-telemetry causes but does not carry this check's key.
Fix: the ec2 data plane's volume attachment must present the volume to the guest as an NVMe device whose serial is the volume id with hyphens removed (and expose the matching `/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_<serial>` link), and the guest command path must return the command's real stdout.
Traces: cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-series-follows-the-volume-across-a-move-to-a-second-instance.

## Evidence
cloudwatch__ec2__ssm.obs-ec2-2-ebs-write-series-follows-the-volume-across-a-move-to-a-second-instance: `AssertionError: the device behind /dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_vol<serial> on i-<id> is not vol-<id>: stdout='EBS-SERIAL \nEBS-IID i-<id>\nEBS-DD \n'`. The volume and instance ids are per-run values; the requirement is that the serial equal the attached volume's id with hyphens removed and that the guest command return its real output.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Launch an instance, create and attach an EBS volume to it.
3. Run a command in the guest that reads `/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_*` and writes a block to the device — the serial field and the write field come back empty, so the guest device cannot be matched to the volume id.
