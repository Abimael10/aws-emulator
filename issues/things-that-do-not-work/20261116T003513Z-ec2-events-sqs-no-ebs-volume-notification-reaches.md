Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__events__sqs.xsvc-ec2-1-attach-and-detach-emit-no-volume-notification-unlike-create
Issue-Key: ec2__events__sqs.xsvc-ec2-1-attach-and-detach-emit-no-volume-notification-unlike-create|_drain#28|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: e65249a4cc18a69ccbdd2b8121120234b78ddf4a

# ec2__events__sqs: no "EBS Volume Notification" reaches an EventBridge rule, so the attach/detach-emit-nothing negative cannot be graded (check times out)

> _Note: this issue was identified against the code base at commit `e65249a4cc18a69ccbdd2b8121120234b78ddf4a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachVolume

## Description
Cause 1 of 1: EC2 delivers no "EBS Volume Notification" envelope to an EventBridge rule's SQS target, so the positive createVolume delivery never arrives and the negative — that a successful AttachVolume/DetachVolume emits no notification, and that a rule pinned to detail.event "attachVolume" therefore receives nothing — can never be distinguished from "EBS events do not work at all". Failing check: ec2__events__sqs.xsvc-ec2-1-attach-and-detach-emit-no-volume-notification-unlike-create.

AWS: the EC2 Smithy model declares no errors and no event schema for CreateVolume/AttachVolume/DetachVolume/DescribeVolumes (smithy_query on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json reports `errors: []` for all four), so this is recorded behavior only. What the recording shows: CreateVolume emits exactly one EventBridge envelope with source `aws.ec2` and detail-type `EBS Volume Notification`, whose `detail.event` is `createVolume`, `detail.result` is `available`, `detail.cause` is the empty string, and `detail.request-id` is the non-empty request id of the CreateVolume call; its `resources` list names the volume ARN. A SUCCESSFUL AttachVolume and a successful DetachVolume emit NO EBS Volume Notification at all — for a volume that was created, attached and detached, an unpinned rule matching only {source, detail-type} delivers exactly the set {createVolume} and nothing else, and a rule additionally pinned to `detail: {"event": ["attachVolume"]}` delivers nothing for that volume (not the createVolume either, because the pattern does not match it). Re-patterning the pinned rule to `createVolume` makes it start delivering, which is what proves the rule itself was live rather than broken.

Emulator: nothing is delivered to either rule's SQS target. The check blocks in its first delivery barrier — the poll for the subject volume's `createVolume` envelope on the unpinned rule (in-check budget 300s) — and the replay reports only `Error: Check timed out after 60.0s`. Because the positive never arrives, the emulator's silence on attach/detach is indistinguishable from total absence of EBS volume eventing, and none of the envelope-shape requirements (result `available`, empty cause, non-empty request-id, volume ARN in resources) nor the pinned-rule negative nor the re-patterning repair are ever evaluated.

Why: a client that keys EBS automation off volume notifications gets no event for a created volume, so it waits forever; and a client that relies on attach/detach being silent cannot verify that contract, because the emulator is silent for every event rather than only for attach and detach. A "no event was emitted" assertion passes vacuously against this emulator for the wrong reason, which makes the eventing surface untestable.

Fix: EC2's volume-lifecycle event emission into the EventBridge bus — the CreateVolume handler must publish one `EBS Volume Notification` envelope (detail.event createVolume, result available, empty cause, the call's request-id, the volume ARN in resources) while the AttachVolume and DetachVolume handlers publish none on success.

Related open issue, which does NOT cover this check: 20261107T075601Z-ec2-events-sqs-ec2-eventbridge-volume-instance-lifecycle-notifications-never-arrive.md lists this check id in its trace ids, but the replay did NOT mark this failure as owned by it (its recorded replay key no longer matches), so the failure is live and unowned and is filed here; a fixer closing that issue should re-replay this check id as well.

Traces: ec2__events__sqs.xsvc-ec2-1-attach-and-detach-emit-no-volume-notification-unlike-create.

## Evidence
From the behavioral replay (### ec2__events__sqs):

[mismatch] ec2__events__sqs.xsvc-ec2-1-attach-and-detach-emit-no-volume-notification-unlike-create
Error: Check timed out after 60.0s

Expected (recorded AWS behavior, as asserted by the check):
- Unpinned rule {"source":["aws.ec2"],"detail-type":["EBS Volume Notification"]}: for the subject volume the delivered set of detail.event values is exactly ["createVolume"] — a successful AttachVolume and DetachVolume add nothing.
- Every delivered envelope for the subject and barrier volumes has (detail.event, detail.result, detail.cause) == ("createVolume", "available", "") and a non-empty string detail.request-id.
- Rule pinned to detail {"event":["attachVolume"]}: delivers nothing for either volume; re-patterning it to createVolume makes it deliver.

Actual: zero envelopes delivered to either queue. The check never clears its first barrier (the poll for the subject volume's createVolume notification on the unpinned rule) and is killed by the harness timeout, so the attach/detach negative and the pinned-rule negative are never graded.

## Steps to Reproduce
1. Create a VPC/subnet and launch a t3.micro instance; wait for it to reach "running".
2. Create two SQS queues and two ENABLED default-bus EventBridge rules, each with a queue policy allowing events.amazonaws.com sqs:SendMessage for the rule ARN and the queue as its target: rule A with EventPattern {"source":["aws.ec2"],"detail-type":["EBS Volume Notification"]}, rule B with the same plus "detail": {"event": ["attachVolume"]}.
3. CreateVolume in the instance's AZ, then poll queue A for an envelope whose resources name the volume ARN and whose detail.event is "createVolume". Observed: no messages arrive.
4. AttachVolume the volume to the instance (/dev/sdf), wait for in-use/attached, then DetachVolume and wait for "available".
5. Create a second (barrier) volume and poll queue A for its createVolume envelope, then drain both queues.
6. Expect: queue A delivered exactly one createVolume envelope per volume (result "available", empty cause, non-empty request-id) and nothing for the attach or the detach; queue B delivered nothing.
Replay: ec2__events__sqs.xsvc-ec2-1-attach-and-detach-emit-no-volume-notification-unlike-create.
