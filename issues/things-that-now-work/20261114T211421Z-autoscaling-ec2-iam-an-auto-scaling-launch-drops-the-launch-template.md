Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__iam.e2e-asgeni-secondary-eni-attach-failure-lifecycle
Issue-Key: autoscaling__ec2__iam.e2e-asgeni-secondary-eni-attach-failure-lifecycle|phase#6|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: a96ded234c891227942f65156e56a199ba16999f

# An Auto Scaling launch drops the launch template's BlockDeviceMappings Ebs.VolumeSize: the instance boots an 8 GiB root volume instead of the templated 100 GiB

> _Note: this issue was identified against the code base at commit `a96ded234c891227942f65156e56a199ba16999f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Cause 1 of 1: the instance an Auto Scaling group launches from a launch template does not get the root volume the template's BlockDeviceMappings describe — the mapping's Ebs.VolumeSize is dropped.
  AWS:      recorded behavior only (the Auto Scaling model has no member for this; it is EC2's launch-template contract). A launch template whose LaunchTemplateData.BlockDeviceMappings contains {DeviceName: /dev/xvda, Ebs: {VolumeSize: 100, VolumeType: gp3, DeleteOnTermination: true}} overrides the AMI's block-device mapping for that device. An instance that an Auto Scaling group launches from that template (or version) therefore reports a /dev/xvda mapping whose EBS volume describes, via DescribeVolumes, with Size equal to the template's VolumeSize and VolumeType equal to the template's VolumeType — in general: every field of the template's mapping for a device is applied to the created volume, with the AMI's value used only for fields the template leaves unset. No refusal is involved; the recorded call sequence succeeds with HTTP 200 throughout.
  Emulator: the group scales out and the instance reaches Pending/InService with the right subnet, security group, instance profile, IMDSv2 setting and propagated tags, and it does expose a /dev/xvda mapping with a volume — but that volume describes as Size=8 with VolumeType=gp3. The templated VolumeType is honoured while the templated VolumeSize is not: the volume is created at the AMI's default size instead. Hypothesis: the Auto Scaling launch path (or the EC2 RunInstances handler it calls) copies only part of each BlockDeviceMappings entry when it materialises the volume, so Ebs.VolumeSize never reaches volume creation.
  Why:      a client cannot verify the storage its group actually launches. Capacity-sizing assertions, disk-space preflight checks and remediation that resizes or inspects a root volume all read 8 GiB from the emulator and a template-sized volume from AWS; worse, the templated size is silently lost rather than refused, so there is no signal that the configuration was ignored. It also halts the whole secondary-ENI diagnosis lifecycle at its second phase, so the remaining four phases of that behaviour are never exercised.
  Fix:      the Auto Scaling scale-out launch path that composes the EC2 instance request from the resolved launch-template version, and the EC2 RunInstances/volume-creation state it hands off to: every Ebs field of a BlockDeviceMappings entry (VolumeSize, VolumeType, DeleteOnTermination, Iops/Throughput, Encrypted/KmsKeyId) must be applied to the created volume, falling back to the AMI's mapping only where the template omits the field.
  Traces:   autoscaling__ec2__iam.e2e-asgeni-secondary-eni-attach-failure-lifecycle (phase 2/6 pre_invoke).

Note on attribution: the suite lists this check under the composite heading `autoscaling__ec2__iam`; the defect sits on the boundary between the Auto Scaling launch path and EC2's instance/volume state, and both of those services are bootstrapped.

## Evidence
Check autoscaling__ec2__iam.e2e-asgeni-secondary-eni-attach-failure-lifecycle, phase 2/6 (pre_invoke):
  AssertionError: [phase 2/6 pre_invoke] i-00000000000000001's root volume is (8, 'gp3'), expected (100, 'gp3')

The stack's launch template (deployments/asgeni-helper/template.json, resource HelperLT) declares:
  "BlockDeviceMappings": [{"DeviceName": "/dev/xvda", "Ebs": {"VolumeSize": 100, "VolumeType": "gp3", "DeleteOnTermination": true}}]
and the group HelperASG launches from that template id at its LatestVersionNumber.

expected: DescribeVolumes on the /dev/xvda volume of the ASG-launched instance -> (Size, VolumeType) == (100, "gp3")
actual:   (8, "gp3") — the templated VolumeType survives, the templated VolumeSize does not.
(The instance's subnet, security group, IamInstanceProfile, MetadataOptions.HttpTokens=required, propagated tags and single primary ENI all matched; only the volume size diverged.)

## Steps to Reproduce
1. Create a launch template whose LaunchTemplateData sets ImageId, InstanceType=t3.micro and BlockDeviceMappings=[{DeviceName: "/dev/xvda", Ebs: {VolumeSize: 100, VolumeType: "gp3", DeleteOnTermination: true}}].
2. Create an Auto Scaling group on that template id/version with MinSize=0, MaxSize=1, DesiredCapacity=0 in one subnet.
3. UpdateAutoScalingGroup(MinSize=1, MaxSize=1, DesiredCapacity=1) and poll DescribeAutoScalingGroups until one instance is Pending/InService.
4. DescribeInstances on that instance, take the BlockDeviceMappings entry for /dev/xvda, and DescribeVolumes its VolumeId.
5. Observe Size=8 instead of the template's 100 (VolumeType is gp3 either way).

Replay: run_behavioral_suite and re-run autoscaling__ec2__iam.e2e-asgeni-secondary-eni-attach-failure-lifecycle.
