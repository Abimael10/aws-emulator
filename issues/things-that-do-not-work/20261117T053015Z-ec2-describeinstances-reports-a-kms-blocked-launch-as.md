Issue-Source: trace-sweep-divergence
Issue-Trace: asglf-disabled-kms-key-launch-cancelled
Issue-Step: task#17
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: asglf-disabled-kms-key-launch-cancelled
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.stateReason.code
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.stateReason.message
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.requesterId
Issue-Related: 20261103T071133Z-ec2-describeinstances-and-launch-template-projections-omit-modeled.md
Issue-Related: 20261111T180943Z-ec2-read-paths-serve-emulator-derived-catalog-data.md
Issue-Commit: 57917fbf623074cc27ff2073937536de1df2e679

# ec2 DescribeInstances reports a KMS-blocked launch as a user-initiated shutdown and omits the launch-failure projection

> _Note: this issue was identified against the code base at commit `57917fbf623074cc27ff2073937536de1df2e679`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
One cause. An instance whose launch was cancelled because its EBS root volume names a customer CMK that is not usable is described with the wrong state-change reason and a projection shaped like a healthy, then-terminated instance.

AWS: Smithy: DescribeInstances -> Instance.stateReason -> com.amazonaws.ec2#StateReason {code, message}. The model documents the state-change codes, including 'Client.InternalError': 'A client error caused the instance to terminate during launch.' For a launch that EC2 itself cancelled because the KMS key for the root volume is in an unusable state, the recorded projection is: instancesSet.item.reason = 'Client.InternalError'; stateReason.code = 'Client.InvalidKMSKey.InvalidState'; stateReason.message = 'Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state' (fixed text — stateReason.message is the code, a colon-space, and the reason sentence, with no per-run values); privateDnsName present and EMPTY (no DNS name was ever assigned); blockDeviceMapping absent (no volume was ever attached); metadataOptions.state = 'pending' (the launch never completed, so the options were never applied); clientToken and reservationSet.item.requesterId present.
Emulator: the same read reports reason 'User initiated (<timestamp>)', stateReason.code 'Client.UserInitiatedShutdown', stateReason.message 'Client.UserInitiatedShutdown: User initiated shutdown', a fully populated blockDeviceMapping (an attached /dev/xvda EBS volume), metadataOptions.state 'applied', an empty clientToken, no privateDnsName member and no requesterId — i.e. it models the instance as having launched normally and then been shut down by the caller.
Why: the only machine-readable evidence of WHY the launch failed is stateReason.code, and a client reading it is told the user terminated the instance. Nothing distinguishes an unusable-CMK cancellation from a deliberate terminate, so retry/remediation logic that keys off Client.InvalidKMSKey.InvalidState (re-enable or re-grant the key) never runs, and the reported attached root volume contradicts a launch that never provisioned storage.
Fix: the EC2 instance state in the cancelled-launch path plus the DescribeInstances projection — record the cancellation's own stateReason instead of the user-initiated-shutdown default, suppress the block-device mapping for an instance that never attached a volume, keep metadataOptions.state at 'pending', and emit the empty privateDnsName and the requesterId members.
Related, non-covering open issues: 20261111T024602Z-autoscaling-ec2-kms-an-asg-scale-out-from.md and 20261115T121847Z-autoscaling-ec2-kms-an-auto-scaling-launch-under.md both concern the autoscaling-side scaling activity being recorded Successful; neither carries this ec2 DescribeInstances step's replay key, and this read diverges even once the activity is fixed.
Traces: asglf-disabled-kms-key-launch-cancelled#task#17.

## Evidence
Fixture asglf-disabled-kms-key-launch-cancelled, step task#17, ec2 DescribeInstances, status 200 expected and 200 got. Field-level divergences:
- instancesSet.item.reason: expected 'Client.InternalError', got 'User initiated (<run timestamp>)'
- instancesSet.item.stateReason.code: expected 'Client.InvalidKMSKey.InvalidState', got 'Client.UserInitiatedShutdown'
- instancesSet.item.stateReason.message: expected 'Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state', got 'Client.UserInitiatedShutdown: User initiated shutdown'
- instancesSet.item.privateDnsName: expected present-and-empty, got missing
- instancesSet.item.blockDeviceMapping: expected absent, got an attached /dev/xvda EBS mapping
- instancesSet.item.metadataOptions.state: expected 'pending', got 'applied'
- instancesSet.item.clientToken: expected present (any value), got ''
- reservationSet.item.requesterId: expected present (any value), got missing
The timestamps and volume/instance ids in the actual response are per-run values and are not themselves the finding; the state-reason code and its fixed message, and the presence/absence of the listed members, are.

## Steps to Reproduce
1. Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator.
2. Fixture asglf-disabled-kms-key-launch-cancelled: a launch template version whose EBS root volume names a disabled customer CMK is used to scale an Auto Scaling group out, so the instance launch is cancelled.
3. At step task#17 call ec2 DescribeInstances for the resulting instance and inspect reservationSet.item.instancesSet.item: stateReason.code must be 'Client.InvalidKMSKey.InvalidState' with message '<code>: The KMS key provided is in an incorrect state', reason must be 'Client.InternalError', privateDnsName must be present and empty, blockDeviceMapping must be absent, metadataOptions.state must be 'pending', and reservationSet.item.requesterId must be present.
