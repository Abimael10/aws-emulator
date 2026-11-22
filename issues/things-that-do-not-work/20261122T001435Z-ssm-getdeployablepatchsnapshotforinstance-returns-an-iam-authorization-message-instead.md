Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-deployable-patch-snapshot-invalid-instance
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-deployable-patch-snapshot-invalid-instance ssm-get-deployable-patch-snapshot-for-unknown-instance
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|GetDeployablePatchSnapshotForInstance|4xx->4xx|$.Message
Issue-Commit: 29c63349654daecf518dc89fab6ca09926c7725f

# SSM GetDeployablePatchSnapshotForInstance returns an IAM authorization message instead of the instance/credential mismatch refusal

> _Note: this issue was identified against the code base at commit `29c63349654daecf518dc89fab6ca09926c7725f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployablePatchSnapshotForInstance

## Description
Cause 1 of 1: GetDeployablePatchSnapshotForInstance called with an instance id that is syntactically valid but is not an instance the caller's credentials resolve to is refused with the wrong error message.

AWS: recorded behavior only - the Smithy model (ssm GetDeployablePatchSnapshotForInstance: required InstanceId with pattern ^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$, required SnapshotId, modeled errors InternalServerError / UnsupportedFeatureRequiredException / UnsupportedOperatingSystem) models no error for an instance that does not belong to the caller, so the requirement comes from the recording. The recorded response is HTTP 400 with the message "Instance Id <InstanceId> doesn't match the credentials". The fixed text of that message is "Instance Id " ... " doesn't match the credentials"; the only variable part is the instance id echoed verbatim from the request's InstanceId member. The recorded error code is the one the emulator already returns (the replay reports no code or status divergence - only $.Message), so the shape and status are right and the wording is not.

Emulator: the same request answers HTTP 400 but with the message 'User: arn:aws:iam::<account>:user/<principal> is not authorized to perform: ssm:GetDeployablePatchSnapshotForInstance on resource: arn:aws:ec2:<region>:<account>:instance/<InstanceId>' - a generic IAM authorization-denied sentence synthesized from the caller principal and a constructed EC2 instance ARN, rather than SSM's credential-mismatch refusal. (Principal, account and region in that text are environment values, not part of the requirement; the defect is the message template.)

Why: clients and operators distinguish "this instance is not yours / not registered" from "your IAM policy denies this call". A client matching on the recorded SSM wording, or surfacing the message to a user, concludes the request was blocked by IAM policy and retries after widening permissions, which never resolves an unknown-instance request. Patch-tooling that keys on the message to decide between re-registering the instance and fixing credentials takes the wrong branch.

Fix: the GetDeployablePatchSnapshotForInstance handler's refusal path in the ssm service crate - when the requested InstanceId does not resolve to a registered/owned instance it must emit SSM's instance-credential mismatch message (fixed text with the request's InstanceId interpolated) instead of routing the case through the generic not-authorized message builder. No change to the status (400) or the error code is needed.

Traces: ssm-deployable-patch-snapshot-invalid-instance#task#1 (representative), ssm-get-deployable-patch-snapshot-for-unknown-instance#task#1, ssm-get-deployable-patch-snapshot-for-unknown-instance#task#2 - same cluster, divergence key service=ssm|GetDeployablePatchSnapshotForInstance|status=400->400|keys=$.Message. No open issue covers this cluster (the replay reports it unowned; the open SSM issues concern associations, patch-baseline reads, automation signals, document catalogs and DescribeAvailablePatches).

## Evidence
Fixture ssm-deployable-patch-snapshot-invalid-instance, step task#1 (GetDeployablePatchSnapshotForInstance, instance id not resolvable by the caller):
  Expected: HTTP 400, $.Message = "Instance Id i-00000000deadbeef0 doesn't match the credentials"
  Actual:   HTTP 400, $.Message = "User: arn:aws:iam::000000000000:user/emulator is not authorized to perform: ssm:GetDeployablePatchSnapshotForInstance on resource: arn:aws:ec2:us-west-2:000000000000:instance/i-00000000deadbeef0"
Status and error code agree; only $.Message diverges (value_mismatch). The instance id in both messages is the request's InstanceId - the divergence is the surrounding fixed text, not the id. Cluster affected_count=3: also ssm-get-deployable-patch-snapshot-for-unknown-instance#task#1 and #task#2.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Or directly: POST an SSM GetDeployablePatchSnapshotForInstance request with InstanceId set to a well-formed but unregistered instance id (e.g. i-00000000deadbeef0) and any valid 36-char UUID SnapshotId.
3. Observe the 400 response body: the emulator returns the generic 'User: ... is not authorized to perform: ssm:GetDeployablePatchSnapshotForInstance on resource: ...' message, whereas AWS returns "Instance Id <InstanceId> doesn't match the credentials".
4. Re-replay fixtures ssm-deployable-patch-snapshot-invalid-instance (task#1) and ssm-get-deployable-patch-snapshot-for-unknown-instance (task#1, task#2) to confirm.
