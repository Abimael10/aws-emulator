Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled autoscaling__ec2__kms.asglf-group-launch-under-enabled-key-without-slr-grant-blocked
Issue-Key: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled|run#14|AssertionError|assert act["StatusCode"] == "Cancelled", (
Issue-Key: autoscaling__ec2__kms.asglf-group-launch-under-enabled-key-without-slr-grant-blocked|run#16|AssertionError|assert act["StatusCode"] == "Cancelled", (
Issue-Commit: 6d844bf2d7a54913ccf026b8beb416d1976fa525

# autoscaling__ec2__kms: an Auto Scaling launch under an unusable customer CMK is recorded Successful — neither the key state nor the key policy's authorization of the service-linked role is evaluated (2 causes)

> _Note: this issue was identified against the code base at commit `6d844bf2d7a54913ccf026b8beb416d1976fa525`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
Two net-new root causes in the Auto Scaling launch path, both reached through SetDesiredCapacity on a group whose launch template encrypts the root volume (BlockDeviceMappings[0].Ebs.Encrypted=true, Ebs.KmsKeyId=<customer CMK>). In both, the emulator launches the instance and records the scaling activity as a completed success, where AWS cancels the launch.

Related open issue 20261111T024602Z-autoscaling-ec2-kms-an-asg-scale-out-from.md describes the same family of defect for the `$Default` version-literal check, but it does not carry either replay key below; Cause 1 here is the plain, explicitly-granted-SLR form of the same gap and Cause 2 is a distinct authorization gap it does not mention. Also related, in other services: 20261110T080328Z-ec2-kms-runinstances-ignores-the-kms-key-state-of-the.md and 20261107T075601Z-ec2-events-kms-sqs-an-instance-whose-root-volume-is-under-a.md.

Cause 1 of 2 — a launch under a DISABLED CMK ends Successful instead of Cancelled.
AWS: Auto Scaling calls EC2 RunInstances at launch time; EC2 refuses to create a volume encrypted under a CMK whose KeyState is not Enabled, so the launched instance never reaches InService, is terminated, and the group's scaling activity ends terminally as StatusCode "Cancelled". Smithy: Activity.StatusCode is required and targets the ScalingActivityStatusCode enum whose values include "Cancelled", "Failed" and "Successful"; Activity.StatusMessage is the verbose status and Activity.Progress is the 0..100 completion. Beyond that the model says nothing about the KMS coupling, so the coupling itself is recorded behavior only: the recording has the first terminal activity after SetDesiredCapacity(1) read StatusCode "Cancelled" with Progress 100, and the attempted EC2 instance reach State.Name "terminated" with StateReason.Code "Client.InvalidKMSKey.InvalidState". The fixed text of the refusal, as AWS words it for this class, is: the EC2 state-reason message "Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state" and the activity StatusMessage "Instance became unhealthy while waiting for instance to be in InService state. Termination Reason: Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state" — that whole text is fixed for this class; nothing in it is per-request, while the group name, activity id and instance id that appear in the activity's Description/ActivityId and in the event detail are values taken from the request and from the emulator's state. Re-enabling the key must let the SAME group's own retry reach one InService instance and record a "Successful" activity.
Emulator: the first terminal activity is StatusCode 'Successful' with StatusMessage None — "a launch under a disabled key must end Cancelled, got 'Successful': None". The instance stays, so the follow-on expectations (no InService instance, a terminated attempt whose StateReason.Code is Client.InvalidKMSKey.InvalidState) are unreachable. The key's state is observable to the emulator at launch time: the check asserts kms.describe_key(...).KeyMetadata.KeyState == "Disabled" before the SetDesiredCapacity.
Why: a client that waits on DescribeScalingActivities to learn whether a capacity change landed is told the fleet grew when, on AWS, nothing was launched. Capacity reported as added is phantom, and every remediation path keyed on a Cancelled activity plus the KMS status message (re-enable the key, repoint the template) is never exercised. Worse, the instance the emulator keeps running is one AWS would never have created, so an encryption-at-rest requirement enforced by key state is silently bypassed.
Failing check: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled.

Cause 2 of 2 — a launch under an ENABLED CMK whose key policy does not name the Auto Scaling service-linked role ends Successful instead of Cancelled; the key policy is never consulted.
AWS: a CMK under its default (account-root-only) key policy does not authorize Auto Scaling's service-linked role arn:aws:iam::<account>:role/aws-service-role/autoscaling.amazonaws.com/AWSServiceRoleForAutoScaling to use it, so the launch is refused for authorization reasons and surfaces in exactly the same shape as Cause 1: the scaling activity ends StatusCode "Cancelled" and the attempted instance is terminated with StateReason.Code "Client.InvalidKMSKey.InvalidState". Smithy says nothing about key-policy authorization (recorded behavior only); the recording has the activity Cancelled while the policy names no SLR, and Successful with exactly one InService instance once only the documented SLR statements (kms:CreateGrant plus the Decrypt/GenerateDataKey*/DescribeKey/ReEncrypt* set) are added. The check first proves the precondition from the emulator's own state: get_key_policy(PolicyName="default") contains no statement whose Principal mentions the group's ServiceLinkedRoleARN.
Emulator: "a launch the SLR is not authorized for must end Cancelled, got 'Successful': None" — the launch succeeds under a key whose policy authorizes nobody but the account root, so the key-policy authorization check for the launching principal is not performed at all (this is independent of Cause 1: here the key is Enabled, and the emulator is equally indifferent).
Why: key-policy authorization is the control that keeps an Auto Scaling group from launching under a CMK its role may not use. An emulator that ignores it reports a healthy fleet for a configuration AWS refuses, so a client never learns the grant is missing, and a test that asserts the deny-then-grant lifecycle (the only way to prove the grant is actually required) can never pass.
Failing check: autoscaling__ec2__kms.asglf-group-launch-under-enabled-key-without-slr-grant-blocked.

Fix: the Auto Scaling launch path — the component that performs the EC2 launch for a capacity increase and the scaling-activity recorder that assigns StatusCode/StatusMessage/Progress — must treat the EC2 launch outcome as authoritative and record a terminal "Cancelled" activity with the message above when it fails; and the EC2 RunInstances volume-creation path it calls must consult both the CMK's key state (Cause 1) and the CMK's key-policy authorization of the calling principal (Cause 2) before creating an encrypted root volume, terminating the attempt with StateReason Client.InvalidKMSKey.InvalidState. The group must keep DesiredCapacity at the requested value and retry, so that repairing the single broken thing (EnableKey, or granting the SLR) lets the same group reach InService and record a Successful activity.

Traces: autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled; autoscaling__ec2__kms.asglf-group-launch-under-enabled-key-without-slr-grant-blocked.

## Evidence
Cause 1 — check autoscaling__ec2__kms.asglf-group-launch-under-disabled-key-blocked-until-key-enabled, at the first terminal activity after asg.set_desired_capacity(AutoScalingGroupName=<group>, DesiredCapacity=1) (launch template version 1 has Ebs.Encrypted=true with KmsKeyId=<CMK ARN>; the SLR has been granted; kms.disable_key has run and describe_key reports KeyState=Disabled):
Expected (AWS): Activity.StatusCode == "Cancelled", Progress == 100, StatusMessage == "Instance became unhealthy while waiting for instance to be in InService state. Termination Reason: Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state"; no instance with LifecycleState InService; the attempted EC2 instance State.Name == "terminated" with StateReason.Code == "Client.InvalidKMSKey.InvalidState"; after kms.enable_key the same group reaches exactly one InService instance and its newest activity is "Successful".
Actual (emulator): AssertionError: a launch under a disabled key must end Cancelled, got 'Successful': None (StatusMessage absent).

Cause 2 — check autoscaling__ec2__kms.asglf-group-launch-under-enabled-key-without-slr-grant-blocked, same call, with the CMK Enabled (describe_key reports (KeyState, Enabled) == ("Enabled", True)) and get_key_policy(PolicyName="default") asserted to contain no Principal naming the group's ServiceLinkedRoleARN:
Expected (AWS): Activity.StatusCode == "Cancelled"; the attempted instance terminated with StateReason.Code == "Client.InvalidKMSKey.InvalidState"; after adding only the documented SLR statements to the key policy, exactly one InService instance and a "Successful" activity.
Actual (emulator): AssertionError: a launch the SLR is not authorized for must end Cancelled, got 'Successful': None.

Smithy (com.amazonaws.autoscaling): Activity.StatusCode is required and targets enum ScalingActivityStatusCode = [PendingSpotBidPlacement, WaitingForSpotInstanceRequestId, WaitingForSpotInstanceId, WaitingForInstanceId, PreInService, InProgress, WaitingForELBConnectionDraining, MidLifecycleAction, WaitingForInstanceWarmup, Successful, Failed, Cancelled, WaitingForConnectionDraining, WaitingForInPlaceUpdateToStart, WaitingForInPlaceUpdateToFinalize, InPlaceUpdateInProgress]; Activity.StatusMessage is XmlStringMaxLen255; Activity.Progress is 0..100.

## Steps to Reproduce
Cause 1:
1. kms.CreateKey (symmetric CMK).
2. ec2.CreateLaunchTemplate (version 1) with LaunchTemplateData.BlockDeviceMappings[0] = {DeviceName: <root device>, Ebs: {Encrypted: true, KmsKeyId: <CMK ARN>, VolumeSize: 8, VolumeType: gp3}}.
3. autoscaling.CreateAutoScalingGroup with LaunchTemplate={LaunchTemplateId: <lt>, Version: "1"}, MinSize 0 / MaxSize 1 / DesiredCapacity 0, VPCZoneIdentifier=<private subnet>.
4. Read the group's ServiceLinkedRoleARN and add the documented Auto Scaling statements for it to the CMK's key policy (so authorization is NOT the variable).
5. kms.DisableKey(KeyId=<CMK>); confirm kms.DescribeKey reports KeyState="Disabled".
6. autoscaling.SetDesiredCapacity(AutoScalingGroupName=<group>, DesiredCapacity=1) and poll DescribeScalingActivities for the first activity whose StatusCode is one of Successful/Cancelled/Failed.
   -> AWS: "Cancelled", Progress 100, StatusMessage "Instance became unhealthy while waiting for instance to be in InService state. Termination Reason: Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state"; no InService instance; ec2.DescribeInstances shows the attempt terminated with StateReason.Code "Client.InvalidKMSKey.InvalidState". Emulator: "Successful", no StatusMessage, instance kept.
7. Drain, kms.EnableKey, SetDesiredCapacity(1) again -> AWS: one InService instance and a "Successful" newest activity.

Cause 2:
1-3. As above, but leave the CMK ENABLED and do NOT touch its key policy (default account-root-only policy).
4. Confirm kms.GetKeyPolicy(KeyId=<CMK>, PolicyName="default") names no principal matching the group's ServiceLinkedRoleARN.
5. autoscaling.SetDesiredCapacity(AutoScalingGroupName=<group>, DesiredCapacity=1) and poll for the first terminal activity.
   -> AWS: "Cancelled" and the attempt terminated with StateReason.Code "Client.InvalidKMSKey.InvalidState". Emulator: "Successful".
6. Drain, add only the documented SLR statements to the key policy, SetDesiredCapacity(1) -> AWS: one InService instance and a "Successful" activity.
