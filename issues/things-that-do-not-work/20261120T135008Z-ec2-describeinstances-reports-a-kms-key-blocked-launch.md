Issue-Source: trace-sweep-divergence
Issue-Trace: asglf-disabled-kms-key-launch-cancelled
Issue-Step: task#17
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: asglf-disabled-kms-key-launch-cancelled
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.clientToken
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.reason
Issue-Related: 20261117T053015Z-ec2-describeinstances-reports-a-kms-blocked-launch-as.md
Issue-Commit: 2934be5e771e52682b61be50f4141d9a3e4ec915

# ec2 DescribeInstances reports a KMS-key-blocked launch as a user-initiated shutdown

> _Note: this issue was identified against the code base at commit `2934be5e771e52682b61be50f4141d9a3e4ec915`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
Cause 1 of 1: an EC2 instance whose launch EC2 itself cancelled because the launch template's EBS encryption KMS key is disabled is projected by DescribeInstances as an ordinary user-initiated shutdown, and the reservation envelope for an Auto Scaling launch is incomplete.

AWS: when a launch fails because the KMS key used for EBS encryption is not in an enabled state, EC2 terminates the instance and records the failure on the instance's state reason: Instance.StateReason.Code = 'Client.InvalidKMSKey.InvalidState' and Instance.StateReason.Message = 'Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state' (the whole message is fixed text for this error class — the code, a colon, and the sentence; there are no request- or state-derived parts in it). Instance.StateTransitionReason carries the bare launch-failure class 'Client.InternalError' (fixed text, no timestamp and no parenthetical). Smithy: com.amazonaws.ec2#StateReason members Code (xmlName 'code') and Message (xmlName 'message'); the StateReason.Message documentation enumerates these launch/termination classes, including 'Client.InternalError: A client error caused the instance to terminate during launch.'. Smithy: Instance.StateTransitionReason — 'The reason for the most recent state transition. This might be an empty string.' For an instance launched on the caller's behalf by Auto Scaling, the reservation also carries Reservation.RequesterId (Smithy: com.amazonaws.ec2#Reservation.RequesterId, xmlName 'requesterId' — 'The ID of the requester that launched the instances on your behalf (for example, Amazon Web Services Management Console or Auto Scaling)') and the instance carries the idempotency ClientToken that Auto Scaling supplied on the launch; both are present (shaped, value not asserted) in the recording.

Emulator: the same DescribeInstances step answers 200 but describes a completely different termination. It reports stateReason.code 'Client.UserInitiatedShutdown', stateReason.message 'Client.UserInitiatedShutdown: User initiated shutdown' and reason 'User initiated (<timestamp> GMT)' — i.e. the KMS-key failure class is replaced by the normal user-shutdown class. In the same reply reservationSet.item.requesterId is absent entirely, and instancesSet.item.clientToken is serialized as the empty string instead of the token the launch carried. (The per-run timestamp inside the emulator's reason string is not itself the finding; the diverging class and message are.)

Why: a client that reads an instance's state reason to find out why an Auto Scaling launch produced no running capacity is told the instance was shut down on purpose. Nothing in the reply mentions the disabled KMS key, so the failure is indistinguishable from a deliberate stop/terminate — remediation logic keyed on Client.InvalidKMSKey.InvalidState (re-enable the key, swap the template's encryption key) never fires, and retry loops keyed on 'user initiated' back off forever. The missing requesterId additionally makes an Auto Scaling launch look like a direct RunInstances call, so attribution of instances to the group that launched them fails.

Fix: the EC2 instance-termination path in the service crate's state — a launch cancelled by a KMS key in a non-enabled state must record the InvalidKMSKey.InvalidState state reason and the Client.InternalError transition reason rather than defaulting to the user-initiated-shutdown reason — plus DescribeInstances' Reservation serializer, which must emit requesterId for an Auto-Scaling-originated reservation and the launch's real clientToken instead of an empty element.

Related but not covering: the open issue 'EC2 read paths report resource state that does not match what was created' (20261120T081949Z-ec2-read-paths-report-resource-state-that-does.md) covers other read-path state divergences and does not carry this fixture step or the KMS-key termination class.

Traces: asglf-disabled-kms-key-launch-cancelled#task#17 (DescribeInstances, expected 200 / actual 200, five diverging members).

## Evidence
Fixture asglf-disabled-kms-key-launch-cancelled, step task#17, ec2 DescribeInstances, expected 200 / actual 200:
- $.DescribeInstancesResponse.reservationSet.item.instancesSet.item.stateReason.code: expected 'Client.InvalidKMSKey.InvalidState', got 'Client.UserInitiatedShutdown'
- $.DescribeInstancesResponse.reservationSet.item.instancesSet.item.stateReason.message: expected 'Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state', got 'Client.UserInitiatedShutdown: User initiated shutdown'
- $.DescribeInstancesResponse.reservationSet.item.instancesSet.item.reason: expected 'Client.InternalError', got 'User initiated (<per-run timestamp> GMT)'
- $.DescribeInstancesResponse.reservationSet.item.instancesSet.item.clientToken: expected a shaped (non-empty) token, got ''
- $.DescribeInstancesResponse.reservationSet.item.requesterId: expected present (shaped), got None (member omitted)
Smithy (services/service-com-amazonaws-ec2-amazonec2): StateReason.Code/.Message, Instance.StateTransitionReason, Reservation.RequesterId as cited in the description.

## Steps to Reproduce
1. Replay wire fixture asglf-disabled-kms-key-launch-cancelled against the emulator.
2. The fixture creates a launch template whose EBS block-device mapping encrypts with a KMS key that is then disabled, points an Auto Scaling group at it and drives DesiredCapacity to 1, so EC2 cancels the launch.
3. At step task#17 it calls ec2 DescribeInstances for the resulting instance.
4. Compare the instance's stateReason.code/stateReason.message/reason and the reservation's requesterId and the instance's clientToken against the recorded response: the emulator reports the Client.UserInitiatedShutdown class, omits requesterId and serializes an empty clientToken.
