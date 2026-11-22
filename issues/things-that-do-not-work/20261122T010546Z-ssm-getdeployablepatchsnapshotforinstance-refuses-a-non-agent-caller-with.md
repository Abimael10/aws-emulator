Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.get-deployable-patch-snapshot-unsupported-os-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.get-deployable-patch-snapshot-unsupported-os-rejected|run#8|AssertionError|assert error["Message"] == (
Issue-Commit: cb813a48348861c9fee7799d678fe40fa2b5af87

# SSM GetDeployablePatchSnapshotForInstance refuses a non-agent caller with a generic IAM authorization message instead of AWS's "Instance Id <id> doesn't match the credentials"

> _Note: this issue was identified against the code base at commit `cb813a48348861c9fee7799d678fe40fa2b5af87`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployablePatchSnapshotForInstance

## Description
Cause 1 of 1: GetDeployablePatchSnapshotForInstance, called by an ordinary caller (not the SSM Agent acting as the instance) for a syntactically valid instance id that the caller's credentials do not resolve to, is refused with the wrong error message. Failing check: ssm.get-deployable-patch-snapshot-unsupported-os-rejected.

AWS: recorded behavior only for the message. Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json) models GetDeployablePatchSnapshotForInstance with required InstanceId (pattern ^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$) and required SnapshotId (length 36, UUID pattern), and models only InternalServerError / UnsupportedFeatureRequiredException / UnsupportedOperatingSystem — it models no error for "this instance is not the caller", so the requirement comes from the recording. AWS answers HTTP 400 with error code AccessDeniedException and the message "Instance Id <InstanceId> doesn't match the credentials". In that message the fixed text is "Instance Id " ... " doesn't match the credentials"; the ONLY variable part is the instance id, echoed verbatim from the request's InstanceId member. AWS performs this caller/instance identity check BEFORE any OS validation, so even an instance that would otherwise trip UnsupportedOperatingSystem is refused this way.

Emulator: returns the correct error code (AccessDeniedException) and status, but the message is a generic IAM authorization-denied sentence synthesized from the caller principal and a constructed EC2 instance ARN: "User: arn:aws:iam::<account>:user/<principal> is not authorized to perform: ssm:GetDeployablePatchSnapshotForInstance on resource: arn:aws:ec2:<region>:<account>:instance/<InstanceId>". (The principal, account and region in that text are per-environment values and are not part of any requirement; the defect is the message template itself, not those values.) The check asserts the message and fails on it; the positive control (DescribeAvailablePatches) succeeds before and after, so nothing else in the service is involved.

Why: clients and operators distinguish "this instance is not yours / not a managed node" from "your IAM policy denies this call". A client or patch-management tool that matches on SSM's recorded wording, or surfaces the message to a human, concludes the request was blocked by an IAM policy and retries after widening permissions — which never resolves an unknown/unowned-instance request — instead of re-registering or correcting the instance id. Checks that assert the refusal wording of this operation therefore fail even though the status and error code are right.

Fix: the refusal path of the GetDeployablePatchSnapshotForInstance handler in the ssm service crate. When the requested InstanceId does not resolve to an instance the caller's credentials represent, it must emit SSM's credential-mismatch message (the fixed text with the request's InstanceId interpolated) rather than routing the case through the generic not-authorized message builder; the identity check must also run before OS validation. No change to the HTTP status (400) or to the error code (AccessDeniedException) is required.

Context on already-filed work: the resolved issue 20261120T214510Z-ssm-getdeployablepatchsnapshotforinstance-stub.md implemented this operation out of scaffold-stub state, and the call is no longer an unimplemented stub — the remaining divergence is purely the refusal message wording, not refusal-versus-success. The open WIRE issue 20261122T001435Z-ssm-getdeployablepatchsnapshotforinstance-returns-an-iam-authorization-message-instead.md describes the same message template defect but carries only wire fixture keys (ssm-deployable-patch-snapshot-invalid-instance, ssm-get-deployable-patch-snapshot-for-unknown-instance); it does not carry this behavioral check's replay key, and the replay report lists this check as unowned, so it is filed here with its behavioral replay key attached.

Traces: ssm.get-deployable-patch-snapshot-unsupported-os-rejected.

## Evidence
Failing behavioral check ssm.get-deployable-patch-snapshot-unsupported-os-rejected (verdict: mismatch).

Check body: calls DescribeAvailablePatches(MaxResults=10) as a positive control, then GetDeployablePatchSnapshotForInstance(InstanceId=<well-formed instance id not owned by the caller>, SnapshotId=<random UUID>), asserting Code == "AccessDeniedException" and Message == "Instance Id <InstanceId> doesn't match the credentials".

  Expected: HTTP 400, Code=AccessDeniedException, Message="Instance Id <InstanceId> doesn't match the credentials"  (fixed text "Instance Id "/" doesn't match the credentials"; variable part = request InstanceId)
  Actual:   HTTP 400, Code=AccessDeniedException, Message="User: arn:aws:iam::<account>:user/<principal> is not authorized to perform: ssm:GetDeployablePatchSnapshotForInstance on resource: arn:aws:ec2:<region>:<account>:instance/<InstanceId>"

Replay assertion tail:
  File "check_ssm_get_deployable_patch_snapshot_unsupported_os_rejected.py", line 37, in run
    assert error["Message"] == (
  AssertionError: User: arn:aws:iam::000000000000:user/emulator is not authorized to perform: ssm:GetDeployablePatchSnapshotForInstance on resource: arn:aws:ec2:us-west-2:000000000000:instance/i-0123456789abcdef0

The error code and status agree; only the message diverges. The instance id appears in both messages because it is the request's InstanceId — the divergence is the surrounding fixed text, not the id, the account, the region or the principal.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite.
2. Re-replay check ssm.get-deployable-patch-snapshot-unsupported-os-rejected.
3. Or directly: call SSM DescribeAvailablePatches(MaxResults=10) (succeeds), then call GetDeployablePatchSnapshotForInstance with InstanceId set to a well-formed but unowned/unregistered instance id (e.g. i-0123456789abcdef0) and any valid 36-character UUID SnapshotId.
4. Observe the 400 response: the error code is AccessDeniedException (correct) but the message is the generic "User: ... is not authorized to perform: ssm:GetDeployablePatchSnapshotForInstance on resource: ..." sentence, whereas AWS returns "Instance Id <InstanceId> doesn't match the credentials".
5. Confirm DescribeAvailablePatches still succeeds afterwards (it does), isolating the defect to this operation's refusal message.
