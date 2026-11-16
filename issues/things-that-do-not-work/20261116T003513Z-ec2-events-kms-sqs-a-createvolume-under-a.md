Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__events__kms__sqs.xsvc-ec2-2-create-volume-under-disabled-key-emits-one-failed-notification-rejected
Issue-Key: ec2__events__kms__sqs.xsvc-ec2-2-create-volume-under-disabled-key-emits-one-failed-notification-rejected|_drain#21|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: e65249a4cc18a69ccbdd2b8121120234b78ddf4a

# ec2__events__kms__sqs: a CreateVolume under a disabled KMS key produces no "EBS Volume Notification" at all, so the failed/withdrawn volume outcome is never observable (check times out)

> _Note: this issue was identified against the code base at commit `e65249a4cc18a69ccbdd2b8121120234b78ddf4a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateVolume

## Description
Cause 1 of 1: EC2 publishes no "EBS Volume Notification" events onto the EventBridge default bus for CreateVolume, so an asynchronously-failed encrypted volume (disabled CMK) is never reported, never withdrawn, and the whole disabled-key-vs-enabled-key comparison cannot reach a verdict. Failing check: ec2__events__kms__sqs.xsvc-ec2-2-create-volume-under-disabled-key-emits-one-failed-notification-rejected.

AWS: the EC2 Smithy model declares no errors and no event schema for CreateVolume (smithy_query CreateVolume/DescribeVolumes/AttachVolume/DetachVolume on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json all report `errors: []`), so this is recorded behavior only. What the recording shows: CreateVolume with Encrypted=true and a KmsKeyId whose key is in KeyState `Disabled` is ACCEPTED synchronously and returns a volume id with State `creating`; the refusal is asynchronous. EBS then publishes exactly ONE EventBridge envelope with source `aws.ec2`, detail-type `EBS Volume Notification`, `account` = the caller's account and `region` = the request region, whose `detail.event` is `createVolume`, `detail.result` is `failed`, and whose `detail.cause` is the fixed sentence `<key> is disabled.` — fixed text ` is disabled.` with the KMS key ARN from the request substituted. Its `resources` list is `[<volume arn>, <kms key arn>]`, in that order. EBS then withdraws the volume with a second envelope whose `detail.event` is `deleteVolume` and `detail.result` is `deleted`, carrying the SAME `detail.request-id` as the failed createVolume envelope. After the withdrawal, DescribeVolumes on that volume id is refused with error code `InvalidVolume.NotFound` and the message `The volume '<volume-id>' does not exist.` — fixed text `The volume '` / `' does not exist.` with the requested volume id substituted. A volume created under an ENABLED key instead yields exactly one `createVolume` envelope with `detail.result` = `available`, `detail.cause` = the empty string, and `resources` = `[<volume arn>]` only, and is never withdrawn. After EnableKey on the previously disabled key, the identical CreateVolume yields one `createVolume`/`available` envelope and DescribeVolumes reports Encrypted=true with KmsKeyId equal to that key.

Emulator: nothing is ever delivered to the SQS target of an ENABLED default-bus rule matching {"source":["aws.ec2"],"detail-type":["EBS Volume Notification"]}. The check blocks in its first delivery barrier (its polling loop over SQS ReceiveMessage, in-check budget 300s) and the replay reports only `Error: Check timed out after 60.0s`. Because no envelope arrives, none of the downstream requirements above are reached: there is no failed `createVolume`, no `deleteVolume`/`deleted` withdrawal sharing the request-id, no enabled-key `available` control, and the InvalidVolume.NotFound refusal for the withdrawn volume is never exercised.

Why: a client that watches EBS volume notifications to learn that an encryption-key problem killed a volume receives nothing — it sees CreateVolume succeed with State `creating` and then has no signal at all, so it cannot distinguish "volume is still being created", "volume failed because the CMK is disabled" and "EBS events are not wired". Nothing drives the compensating delete, so the failed volume is also never removed from the emulator's view.

Fix: EC2's volume-lifecycle event emission into the EventBridge bus (the CreateVolume handler's asynchronous settlement and its EBS Volume Notification publisher), including the KMS key-state check that settles the volume `failed` with the `<key> is disabled.` cause, the paired `deleteVolume`/`deleted` withdrawal reusing the createVolume request-id, and the resulting InvalidVolume.NotFound state for the withdrawn volume id.

Related open issue, which does NOT cover this check: 20261107T075601Z-ec2-events-sqs-ec2-eventbridge-volume-instance-lifecycle-notifications-never-arrive.md describes volume/instance lifecycle notifications never arriving under service ec2__events__sqs; the replay did not mark this ec2__events__kms__sqs check as owned by it, and the disabled-CMK failed/withdrawn notification pair and its cause text are not described there. 20261115T121847Z-autoscaling-ec2-events-kms-sqs-no-ec2-instance.md covers the Auto Scaling launch-unsuccessful event, not EBS volume notifications.

Traces: ec2__events__kms__sqs.xsvc-ec2-2-create-volume-under-disabled-key-emits-one-failed-notification-rejected.

## Evidence
From the behavioral replay (### ec2__events__kms__sqs):

[mismatch] ec2__events__kms__sqs.xsvc-ec2-2-create-volume-under-disabled-key-emits-one-failed-notification-rejected
Error: Check timed out after 60.0s

Expected (recorded AWS behavior, as asserted by the check):
- CreateVolume(Encrypted=true, KmsKeyId=<disabled key>) accepted, State == "creating".
- Exactly one envelope: source "aws.ec2", detail-type "EBS Volume Notification", detail.event "createVolume", detail.result "failed", detail.cause "<kms key arn> is disabled.", resources ["<volume arn>", "<kms key arn>"].
- Exactly one envelope: detail.event "deleteVolume", detail.result "deleted", same detail.request-id as the failed createVolume.
- DescribeVolumes(VolumeIds=[<failed volume>]) -> code "InvalidVolume.NotFound", message "The volume '<volume-id>' does not exist."
- Enabled-key control: one envelope, detail.result "available", detail.cause "", resources ["<volume arn>"], never withdrawn.
- After EnableKey, the identical CreateVolume -> one createVolume/"available"; DescribeVolumes reports Encrypted=true, KmsKeyId=<key>.

Actual: zero envelopes delivered to the rule's SQS target; the check never leaves its first delivery barrier and is killed by the harness timeout, so no assertion after the barrier is ever evaluated.

## Steps to Reproduce
1. Create two KMS keys (A and B) and DisableKey on A; confirm DescribeKey reports KeyState "Disabled".
2. Create an SQS queue, a default-bus EventBridge rule with EventPattern {"source":["aws.ec2"],"detail-type":["EBS Volume Notification"]} in state ENABLED, a queue policy allowing events.amazonaws.com sqs:SendMessage for that rule ARN, and PutTargets the queue (FailedEntryCount must be 0).
3. CreateVolume(AvailabilityZone=<available az>, Size=1, VolumeType=gp3, Encrypted=true, KmsKeyId=<key B>) — the control — then the same call with KmsKeyId=<key A> (disabled). Both are accepted with State "creating".
4. Poll ReceiveMessage on the queue. Expect a createVolume/available envelope for the key-B volume, and a createVolume/failed envelope plus a deleteVolume/deleted envelope for the key-A volume. Observed: no messages at all.
5. DescribeVolumes on the key-A volume id: expect InvalidVolume.NotFound "The volume '<id>' does not exist."
6. EnableKey on A and repeat the CreateVolume: expect one createVolume/available envelope and DescribeVolumes reporting Encrypted=true with KmsKeyId=<key A>.
Replay: ec2__events__kms__sqs.xsvc-ec2-2-create-volume-under-disabled-key-emits-one-failed-notification-rejected.
