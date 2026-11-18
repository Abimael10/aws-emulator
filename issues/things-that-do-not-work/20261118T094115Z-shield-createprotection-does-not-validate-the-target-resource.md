Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: shield.create-protection-invalid-name-pattern-rejected
Issue-Shape-Id: com.amazonaws.shield#AWSShield_20160616
Issue-Key: shield.create-protection-invalid-name-pattern-rejected|run#9|AssertionError|assert ctrl_code == "InvalidResourceException", f"expected InvalidResourceException, got {ctrl_code!r}"
Issue-Commit: 7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c

# Shield CreateProtection does not validate the target resource: an unresolvable EIP ARN is refused as ResourceNotFoundException instead of InvalidResourceException

> _Note: this issue was identified against the code base at commit `7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateProtection

## Description
One cause.

Cause 1 of 1: CreateProtection with a well-formed Name and a ResourceArn that does not resolve to a real resource is refused with the account-level subscription error rather than with the resource error AWS raises, because the handler has no resource-resolution step at all and its subscription gate is the first precondition it evaluates.

AWS: Smithy CreateProtection (services/service-com-amazonaws-shield-awsshield-20160616/smithy-model/com-amazonaws-shield-awsshield-20160616.json) takes Name (ProtectionName, @length 1..128, @pattern ^[ a-zA-Z0-9_\.\-]*$, required) and ResourceArn (@length 1..2048, @pattern ^arn:aws, required), and models InvalidResourceException (HTTP 400, "Exception that indicates that the resource is invalid. You might not have access to the resource, or the resource might not exist.") alongside InvalidParameterException, LimitsExceededException, ResourceAlreadyExistsException and ResourceNotFoundException — all client errors at HTTP 400. Recorded behavior: a Name outside the modeled pattern is rejected first with HTTP 400 / ValidationException and the message "Error validating the following inputs: name" (fixed text, with the offending member name(s) as the only variable part — the emulator already gets this leg right). With a Name that satisfies the pattern, the SAME request against a nonexistent Elastic IP allocation advances past name validation and is refused with HTTP 400 / InvalidResourceException, message of the form "Failed to validate EIP resource: <ResourceArn> in region: <region>, error: The allocation ID '<allocation-id>' does not exist (Service: AmazonEC2; Status Code: 400; Error Code: InvalidAllocationID.NotFound; Request ID: <request-id>; Proxy: null)". Fixed text: "Failed to validate EIP resource: ", " in region: ", ", error: The allocation ID '", "' does not exist (Service: AmazonEC2; Status Code: 400; Error Code: InvalidAllocationID.NotFound; Request ID: ", "; Proxy: null)". Variable parts, all taken from the request: the ResourceArn verbatim, the region parsed from that ARN's 4th segment, and the allocation id (the ARN's resource segment after the '/'). The embedded Request ID is a per-run value of Shield's internal EC2 call and is not part of the requirement. AWS therefore judges the target resource before it consults the account's Shield Advanced subscription.

Emulator: the valid-Name call is answered HTTP 400 with error code ResourceNotFoundException (the emulator's subscription-missing refusal), not InvalidResourceException. There is no resource-resolution check anywhere in the handler, and InvalidResourceException is never constructed for this operation.

Why: a client is told the wrong thing to fix. ResourceNotFoundException from Shield means "this account has no Shield Advanced subscription — subscribe and retry", so a caller that passed a stale or wrong ARN retries forever or escalates a nonexistent billing/subscription problem, while the real defect is its ARN. Code that catches Shield.Client.exceptions.InvalidResourceException to fall back to another resource, or that distinguishes a bad resource from an unsubscribed account, never fires. It also makes the Name-pattern rejection unverifiable: the control leg that is supposed to prove the first rejection was about the Name collapses into the same subscription error, so a bad Name and a bad ARN are indistinguishable.

Fix: the CreateProtection handler's precondition ordering and its missing InvalidResourceException construction (services/service-com-amazonaws-shield-awsshield-20160616/src/ops/create_protection.rs) — resolve/validate ResourceArn against the protectable-resource set before the subscription-state gate, and render the failure in Shield's "Failed to validate <type> resource: ..." form. NOTE: the open issue 20261030T110306Z-shield-protection-and-drt-error-paths-report-the.md describes this same handler defect as its Cause 2, but it is keyed to wire fixture steps (shield-protection-error-paths#task#1) and carries no behavioral replay key, so it does not cover this check; fixing either should close both, and this issue records the behavioral check to re-replay.

Traces: behavioral check shield.create-protection-invalid-name-pattern-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator, service shield: 17/18 passed; the single failure is [mismatch] shield.create-protection-invalid-name-pattern-rejected.

Check assertions, in order:
1. create_protection(Name='bad!name@@', ResourceArn='arn:aws:ec2:<region>:111122223333:eip-allocation/eipalloc-00000000000000000') -> expected 400 ValidationException with message 'Error validating the following inputs: name'. PASSED.
2. create_protection(Name='<valid name matching ^[ a-zA-Z0-9_\.\-]*$>', ResourceArn=<same ARN>) -> expected 400 InvalidResourceException with message "Failed to validate EIP resource: <arn> in region: <region>, error: The allocation ID '<allocation-id>' does not exist (Service: AmazonEC2; Status Code: 400; Error Code: InvalidAllocationID.NotFound; Request ID: <per-run request id>; Proxy: null)". FAILED:
   AssertionError: expected InvalidResourceException, got 'ResourceNotFoundException'
   (check_shield_create_protection_invalid_name_pattern_rejected.py line 45)

Source in the checkout: services/service-com-amazonaws-shield-awsshield-20160616/src/ops/create_protection.rs lines 196-206 make the subscription-state gate the handler's FIRST precondition and return ResourceNotFoundException(SUBSCRIPTION_NOT_FOUND_MESSAGE, SUBSCRIPTION_RESOURCE_TYPE); lines 208-240 then check only resource-already-protected and the protection count. The ARN is used at line 256 only to derive a resource type (crate::state::protected_resource_type) when the protection is inserted — it is never validated as existing, and InvalidResourceException is declared in CreateProtectionErrorModeled (line 122) but constructed nowhere in the handler. Message constants live in services/service-com-amazonaws-shield-awsshield-20160616/src/state.rs.

Smithy CreateProtection errors: InternalErrorException (500), InvalidOperationException, InvalidParameterException, InvalidResourceException, LimitsExceededException, OptimisticLockException, ResourceAlreadyExistsException, ResourceNotFoundException (all 400).

## Steps to Reproduce
Boot the emulator and point a shield client at it (the account holds no active Shield Advanced subscription, which is the state the check replays against).

1. shield.create_protection(Name='bad!name@@', ResourceArn='arn:aws:ec2:<region>:111122223333:eip-allocation/eipalloc-00000000000000000')
   -> expected and observed: 400 ValidationException, message 'Error validating the following inputs: name'.
2. shield.create_protection(Name='<any name matching ^[ a-zA-Z0-9_\.\-]*$>', ResourceArn=<the same ARN as step 1>)
   -> AWS: 400 InvalidResourceException, message "Failed to validate EIP resource: <arn> in region: <region>, error: The allocation ID '<allocation-id>' does not exist (Service: AmazonEC2; Status Code: 400; Error Code: InvalidAllocationID.NotFound; Request ID: <per-run>; Proxy: null)".
   -> Emulator: 400 ResourceNotFoundException (subscription-missing refusal).

Re-replay behavioral check shield.create-protection-invalid-name-pattern-rejected to confirm the cause is closed.
