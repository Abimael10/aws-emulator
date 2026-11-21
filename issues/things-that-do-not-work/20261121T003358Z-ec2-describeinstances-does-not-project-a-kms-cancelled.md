Issue-Source: trace-sweep-divergence
Issue-Trace: asglf-disabled-kms-key-launch-cancelled
Issue-Step: task#17
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: asglf-disabled-kms-key-launch-cancelled
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.instanceState.name
Issue-Related: 20261114T013533Z-ec2-read-paths-serve-no-catalog-records-and.md
Issue-Commit: 7b83df792075b2a128316f9b5135ddba9a81dab1

# ec2 DescribeInstances does not project a KMS-cancelled Auto Scaling launch as a terminated instance

> _Note: this issue was identified against the code base at commit `7b83df792075b2a128316f9b5135ddba9a81dab1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
Cause 1 of 1: after EC2 cancels an Auto Scaling launch because the launch template's EBS encryption KMS key is not in an enabled state, DescribeInstances must show that instance in the terminated state inside a single reservation. The emulator answers 200 but its reservation set does not have that shape and the instance's instanceState.name is not 'terminated'.

AWS: a launch that EC2 cancels for a KMS key in an invalid state ends with the instance TERMINATED — Smithy: com.amazonaws.ec2#InstanceState has members Code (xmlName 'code') and Name (xmlName 'name', enum InstanceStateName), and the Code documentation fixes the mapping of the low byte, '48 : terminated'. So the instance element must carry instanceState {code 48, name 'terminated'}; the accompanying StateReason (com.amazonaws.ec2#StateReason, members code/message) carries the launch-failure class for this cancellation and StateTransitionReason the bare launch-failure class 'Client.InternalError' (Smithy StateReason.Message documentation enumerates these classes, including 'Client.InternalError: A client error caused the instance to terminate during launch'). The recording shows exactly one reservation (reservationSet with a single item, ownerId = the caller's account) holding exactly one instance element, that instance terminated with code 48 / name 'terminated' and stateReason code 'Client.InvalidKMSKey.InvalidState' / message 'Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state' (this message is fixed text for the class: the code, a colon, and the sentence — no request- or state-derived part). Per-run values in the recording (instance id, reservation id, launch time, client token) are shaped-not-asserted and are not requirements.

Emulator: the same DescribeInstances step answers 200, but the replay reports $.DescribeInstancesResponse.reservationSet as a type mismatch against the expected single-reservation/single-instance envelope, and $.DescribeInstancesResponse.reservationSet.item.instancesSet.item.instanceState.name as a value mismatch — i.e. the instance the cancelled launch left behind is not reported in the terminated state (code 48 / 'terminated') and the reservation envelope around it does not match the recorded projection.

Why: a client that waits on or polls an Auto Scaling launch to settle (an EC2 instance-terminated waiter, a capacity reconciler, a cleanup job that only acts on terminated instances) never sees the terminal state, so it waits forever or treats the failed launch as live capacity; and a reservation envelope of the wrong shape breaks clients that read reservationSet.item.instancesSet.item positionally, which is how the ec2Query XML projection is consumed.

Fix: the EC2 instance-lifecycle state in the service crate (the launch-cancellation path must leave the instance in the terminated state with code 48) together with the DescribeInstances Reservation/Instance serializer, which must project that instance as one reservation containing one instance element.

Related but not covering (named per instructions; both carry this same fixture step but other members of the reply, so neither owns this divergence key): 20261120T135008Z-ec2-describeinstances-reports-a-kms-key-blocked-launch.md and 20261117T053015Z-ec2-describeinstances-reports-a-kms-blocked-launch-as.md cover stateReason.code/stateReason.message, reason, clientToken and requesterId on this step; this cause is the instance's instanceState.name and the reservationSet shape, which the replay reports as still diverging and unowned.

Traces: asglf-disabled-kms-key-launch-cancelled#task#17.

## Evidence
Replay of trace set simparity-wire-aws-v1 (90 fixtures, 1066 steps, score 85.3%) reported this cluster with no owning issue and no blocking stub:

ec2 DescribeInstances, status 200 -> 200, divergence key ec2|DescribeInstances|status=200->200|keys=$.DescribeInstancesResponse.reservationSet,$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.instanceState.name
  $.DescribeInstancesResponse.reservationSet: type_mismatch — expected a single reservation item ({'item': {...reservationId, ownerId '000000000000', instancesSet: {'item': {...}}}}) whose one instance carries instanceState {'code': '48', 'name': 'terminated'}, reason 'Client.InternalError' and stateReason {'code': 'Client.InvalidKMSKey.InvalidState', 'message': 'Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state'}; the emulator's reservationSet does not match that shape.
  $.DescribeInstancesResponse.reservationSet.item.instancesSet.item.instanceState.name: value_mismatch — expected 'terminated' (with code 48).
  cluster (1 step): asglf-disabled-kms-key-launch-cancelled#task#17

Model grounding: services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json — com.amazonaws.ec2#InstanceState (Code/Name, '48 : terminated' fixed in the Code docs) and com.amazonaws.ec2#StateReason (code/message, launch-failure classes enumerated in the Message docs).

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from the checkout.
2. Replay wire fixture asglf-disabled-kms-key-launch-cancelled against it. The fixture creates a launch template whose EBS block-device mapping encrypts with a KMS key that is then disabled, points an Auto Scaling group at it and drives DesiredCapacity to 1, so EC2 cancels the launch.
3. At step task#17 it calls ec2 DescribeInstances for the resulting instance.
4. Compare the reply against the recording: there must be exactly one reservation holding exactly one instance element, and that instance's instanceState must be code 48 / name 'terminated'. The emulator's reservationSet has a different shape and the instance's instanceState.name is not 'terminated'.
5. Ignore the per-run instance id, reservation id, launch time and client token — they are shaped-not-asserted in the fixture.
