Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled autoscaling__ec2__kms.asglf-zero-desired-group-records-no-launch-attempt-negative-delivery
Issue-Key: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled|_poll#17|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling__ec2__kms.asglf-zero-desired-group-records-no-launch-attempt-negative-delivery|run#16|AssertionError|assert len(tried) >= 1, (
Issue-Commit: d981cd88b1dd8c88d8ac1558b1954231d6a3ad6f

# autoscaling__ec2__kms: 2 net-new causes — the disabled-CMK group-launch scenario reaches no verdict inside the budget, and a launched instance carries no aws:autoscaling:groupName tag so EC2 can never attribute it to its group

> _Note: this issue was identified against the code base at commit `d981cd88b1dd8c88d8ac1558b1954231d6a3ad6f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
Two net-new root causes on the Auto Scaling capacity-increase path for a group whose launch template encrypts the root volume (BlockDeviceMappings[0].Ebs.Encrypted=true, Ebs.KmsKeyId=<customer CMK>). Both are reached through autoscaling.SetDesiredCapacity followed by DescribeScalingActivities and ec2.DescribeInstances.

Related open issues, neither of which carries the replay keys below: 20261115T121847Z-autoscaling-ec2-kms-an-auto-scaling-launch-under.md lists check id asglf-group-launch-under-disabled-key-blocked-until-key-enabled under an EARLIER signature (the Cancelled assertion failing with 'Successful'); the live failure mode of that check is now a budget-exhausting hang, described as Cause 1 and not covered there. 20261114T091241Z-autoscaling-ec2-auto-scaling-3-net-new-causes-setdesiredcapacity-above.md mentions launched instances carrying no launch-template tags; Cause 2 below is the reserved aws:autoscaling:groupName tag, a different tag applied by a different path, and it is not named there.

Cause 1 of 2 — the disabled-CMK launch/repair scenario makes no observable progress and is killed at the suite's 60 s cap, so SetDesiredCapacity never produces an answerable outcome.
AWS: recorded behavior only for the KMS coupling; the model states the shape. Smithy (com.amazonaws.autoscaling): Activity.StatusCode is required and targets enum ScalingActivityStatusCode whose values include "Cancelled", "Failed" and "Successful"; Activity.Progress is 0..100; Activity.StatusMessage is XmlStringMaxLen255. In the recording the whole scenario is observable step by step: after SetDesiredCapacity(DesiredCapacity=1) with the CMK Disabled the group's first terminal activity reads StatusCode "Cancelled" with Progress 100 and the StatusMessage AWS words for this class, "Instance became unhealthy while waiting for instance to be in InService state. Termination Reason: Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state" (that whole sentence is fixed text for the class; the group name, activity id and instance id that appear in Description/ActivityId are values taken from the request and from service state), the attempted EC2 instance reaches State.Name "terminated" with StateReason.Code "Client.InvalidKMSKey.InvalidState", and after kms.EnableKey the SAME group reaches exactly one InService instance whose newest activity is "Successful". No step of this blocks indefinitely on AWS.
Emulator: the check reaches no verdict at all — "Check timed out after 60.0s". Nothing in the flow converges inside the budget: either the first terminal scaling activity never appears, or (Hypothesis:) the hang is the subsequent poll for "EC2 records the attempted instance as terminated", which can never be satisfied while Cause 2 holds, because that poll reads EC2 through the group-name tag filter.
Why: a client driving capacity through SetDesiredCapacity and waiting on DescribeScalingActivities / DescribeInstances to learn the outcome loops until its own deadline and never learns whether the launch landed or was cancelled. The entire deny-then-repair lifecycle (disable key -> Cancelled -> enable key -> Successful), which is the only way to prove key state is enforced at launch, is unverifiable.
Fix: the Auto Scaling launch path for a capacity increase and the scaling-activity recorder that assigns StatusCode/StatusMessage/Progress — a launch attempt must reach a terminal activity promptly, and the EC2 launch it drives must record the attempted instance's terminal state and StateReason rather than leaving observers polling.
Failing check: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled.

Cause 2 of 2 — an instance launched by a group is not discoverable in EC2 by the reserved aws:autoscaling:groupName tag: the filtered DescribeInstances returns an empty list even after the group's own terminal "Launching a new EC2 instance" activity.
AWS: Auto Scaling tags every instance it launches with the reserved tag aws:autoscaling:groupName whose value is the group's name, so ec2.DescribeInstances with Filters=[{Name: "tag:aws:autoscaling:groupName", Values: [<group name>]}] returns that instance in ANY state (pending, running, shutting-down, terminated). Smithy (com.amazonaws.ec2): DescribeInstancesRequest.Filters documents the filter "tag:<key> - The key/value combination of a tag assigned to the resource. Use the tag key in the filter name and the tag value as the filter value."; the tag's presence on ASG-launched instances is recorded behavior only. In the recording, once the control group's launch attempt has reached a terminal activity whose Description starts "Launching a new EC2 instance", that filter yields at least one instance id.
Emulator: the activity does reach a terminal status and its Description does start "Launching a new EC2 instance", but the filtered DescribeInstances yields nothing — "AssertionError: EC2 holds no instance tagged with the control group's name after its launch attempt: []". So the launch path either creates no EC2 instance that DescribeInstances can see, or never applies the reserved aws:autoscaling:groupName tag (and/or EC2's tag:<key> filter does not match it).
Why: the group-name tag is how every client attributes an EC2 instance to its Auto Scaling group — console views, cost/ownership tooling, cleanup scripts, and any differential that proves a group DID attempt a launch. With it absent, a group's members cannot be found through EC2 at all, and every absence assertion ("the DesiredCapacity=0 group holds no instances") becomes vacuous because the positive control reads empty too.
Fix: the Auto Scaling launch path (the component that creates the EC2 instance for a capacity increase) must apply the reserved tag aws:autoscaling:groupName with the group's name taken from state, and EC2's DescribeInstances tag:<key> filter must match instances carrying it.
Failing check: autoscaling__ec2__kms.asglf-zero-desired-group-records-no-launch-attempt-negative-delivery.

Traces: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled; autoscaling__ec2__kms.asglf-zero-desired-group-records-no-launch-attempt-negative-delivery.

## Evidence
Cause 1 — check autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled (group 0/1/0 over a launch template version 1 with Ebs.Encrypted=true and Ebs.KmsKeyId=<CMK ARN>; the CMK's key policy names the group's ServiceLinkedRoleARN; kms.DisableKey has run and kms.DescribeKey reports KeyState="Disabled"; then autoscaling.SetDesiredCapacity(DesiredCapacity=1)):
Expected (AWS): the first terminal activity is StatusCode "Cancelled", Progress 100, StatusMessage "Instance became unhealthy while waiting for instance to be in InService state. Termination Reason: Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state"; no instance with LifecycleState "InService"; ec2.DescribeInstances shows the attempt State.Name "terminated" with StateReason.Code "Client.InvalidKMSKey.InvalidState"; after kms.EnableKey and SetDesiredCapacity(1) the same group holds exactly one InService instance and its newest activity is "Successful". Every one of these is observable within the scenario's own poll windows.
Actual (emulator): no verdict is reached — the suite reports "Error: Check timed out after 60.0s". The flow produces nothing observable to assert on inside the budget.

Cause 2 — check autoscaling__ec2__kms.asglf-zero-desired-group-records-no-launch-attempt-negative-delivery, at the differential barrier: two groups share one launch template (Ebs.Encrypted=true, KmsKeyId=<CMK ARN>); the control group is raised with autoscaling.SetDesiredCapacity(AutoScalingGroupName=<control>, DesiredCapacity=1) and its first terminal activity is read, then EC2 is queried with ec2.DescribeInstances(Filters=[{Name: "tag:aws:autoscaling:groupName", Values: [<control group name>]}]):
Expected (AWS): at least one instance id is returned (the attempted instance, in whatever state the launch left it), because Auto Scaling tagged it aws:autoscaling:groupName=<control group name>.
Actual (emulator): the terminal activity exists and its Description starts "Launching a new EC2 instance" (that assertion passes), but the tag-filtered DescribeInstances returns no reservations — "AssertionError: EC2 holds no instance tagged with the control group's name after its launch attempt: []".

Smithy: com.amazonaws.autoscaling Activity.StatusCode (required) -> ScalingActivityStatusCode enum incl. Successful | Failed | Cancelled; Activity.Progress 0..100; Activity.StatusMessage XmlStringMaxLen255. com.amazonaws.ec2 DescribeInstancesRequest.Filters documents "tag:<key>" (key/value combination of a tag assigned to the resource) and "instance-state-name" (pending | running | shutting-down | terminated | stopping | stopped).

## Steps to Reproduce
Cause 1:
1. kms.CreateKey (symmetric customer CMK).
2. ec2.CreateLaunchTemplate with LaunchTemplateData.BlockDeviceMappings[0] = {DeviceName: "/dev/xvda", Ebs: {Encrypted: true, KmsKeyId: <CMK ARN>, VolumeSize: 8, VolumeType: "gp3", DeleteOnTermination: true}}, ImageId from the AL2023 SSM parameter, InstanceType t3.micro.
3. autoscaling.CreateAutoScalingGroup with LaunchTemplate={LaunchTemplateId: <lt>, Version: "1"}, MinSize 0 / MaxSize 1 / DesiredCapacity 0, VPCZoneIdentifier=<private subnet>.
4. Read the group's ServiceLinkedRoleARN and kms.PutKeyPolicy adding the documented Auto Scaling statements for it (so authorization is not the variable).
5. kms.DisableKey(KeyId=<CMK>); confirm kms.DescribeKey reports KeyState="Disabled".
6. autoscaling.SetDesiredCapacity(AutoScalingGroupName=<group>, DesiredCapacity=1), then poll autoscaling.DescribeScalingActivities for the first activity whose StatusCode is Successful/Cancelled/Failed and ec2.DescribeInstances for the attempted instance in state "terminated".
   -> AWS: the activity is terminal "Cancelled" with the Client.InvalidKMSKey.InvalidState termination message, and the attempt is terminated with StateReason.Code "Client.InvalidKMSKey.InvalidState". Emulator: nothing observable appears and the scenario runs out its budget (60 s cap) with no verdict.
7. Drain (MinSize/DesiredCapacity 0), kms.EnableKey, SetDesiredCapacity(1) -> AWS: one InService instance and a "Successful" newest activity.

Cause 2:
1-2. As above, but kms.DisableKey the CMK before creating the launch template.
3. autoscaling.CreateAutoScalingGroup twice over the SAME launch template (both 0/1/0): an idle group and a control group.
4. autoscaling.SetDesiredCapacity(AutoScalingGroupName=<control>, DesiredCapacity=1) and poll DescribeScalingActivities until the control group has a terminal activity (its Description starts "Launching a new EC2 instance").
5. ec2.DescribeInstances(Filters=[{Name: "tag:aws:autoscaling:groupName", Values: [<control group name>]}]).
   -> AWS: at least one instance is returned, carrying tag aws:autoscaling:groupName=<control group name>. Emulator: Reservations is empty, so no instance id is found.
