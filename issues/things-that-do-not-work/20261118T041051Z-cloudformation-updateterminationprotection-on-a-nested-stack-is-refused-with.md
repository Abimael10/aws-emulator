Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.cdk-nested-stack-is-tracked-and-cascades-on-delete
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.cdk-nested-stack-is-tracked-and-cascades-on-delete|run#13|AssertionError|assert protect.response["Error"]["Message"] == (
Issue-Commit: 475e8943a7760cafd725d17bda984e6b5cbc86d8

# UpdateTerminationProtection on a nested stack is refused with an emulator-authored message instead of AWS's substack sentence

> _Note: this issue was identified against the code base at commit `475e8943a7760cafd725d17bda984e6b5cbc86d8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateTerminationProtection

## Description
Cause 1 of 1: CloudFormation's nested-stack guard on UpdateTerminationProtection returns the right status and error code but an emulator-authored message, and it leaks a second stack ARN that AWS never mentions. Failing check ids: cloudformation.cdk-nested-stack-is-tracked-and-cascades-on-delete.

AWS: UpdateTerminationProtection is modelled with StackName (StackNameOrId, required) and EnableTerminationProtection (required) and declares NO operation-specific errors (Smithy: cloudformation UpdateTerminationProtection, input UpdateTerminationProtectionInput, output StackId, errors []), so the refusal is the query protocol's common ValidationError and its wording is recorded behavior only. The recorded AWS response for setting termination protection on a NESTED (child) stack is HTTP 400 with Code ValidationError and Message:
  Termination protection cannot be updated due to stack [<stack-id>] being a substack
The fixed text is "Termination protection cannot be updated due to stack [" + "] being a substack"; the only variable part is <stack-id>, the identifier of the nested stack named in the request (the StackId/ARN as the request supplied it). AWS names exactly ONE stack and never names the root stack in this message.

Emulator: the call is refused with HTTP 400 / ValidationError (correct), but the message is a different sentence of the emulator's own, and it names two stacks — the nested stack and the root stack:
  Cannot change termination protection directly on the nested stack [<nested-stack-arn>]. For nested stacks, termination protection is set on the root stack [<root-stack-arn>].
(Only the sentence shape is the requirement; the ARNs, account id, stack names and stack UUIDs in the replayed run are per-run values, not part of the template.) The surrounding assertions pass: the error code is ValidationError, the nested stack stays CREATE_COMPLETE with EnableTerminationProtection=False, and the same call against the root/parent stack is accepted — so only the refusal's wording diverges.

Why: a client that keys off the refusal text — CDK/CloudFormation tooling, retry and remediation logic, and tests that assert the documented sentence — does not recognise the refusal. Because the code is the generic ValidationError, the message is the only thing that distinguishes "this stack is a substack" from any other validation failure, so a caller cannot tell it to re-issue the request against the root stack. The emulator also discloses the root stack's ARN in an error AWS keeps to a single stack id.

Fix: the nested-stack guard inside the CloudFormation UpdateTerminationProtection handler (the CloudFormation service crate, services/service-com-amazonaws-cloudformation-cloudformation) must raise ValidationError with AWS's fixed sentence, interpolating only the stack identifier from the request, and must not append the root stack. Note: the resolved issue 20261022T043551Z-cloudformation-stack-mutation-guards-not-enforced-capabilities-client.md made the guard fire at all; it does not cover this wording, and a resolved issue does not own this failure.

Traces: cloudformation.cdk-nested-stack-is-tracked-and-cascades-on-delete (assertion on protect.response["Error"]["Message"] after UpdateTerminationProtection(StackName=<nested stack id>, EnableTerminationProtection=True)).

## Evidence
Check: cloudformation.cdk-nested-stack-is-tracked-and-cascades-on-delete [mismatch]

Expected (AWS, as the check asserts it):
  HTTP 400
  Error.Code    = ValidationError
  Error.Message = "Termination protection cannot be updated due to stack [{nested_stack_id}] being a substack"
  fixed text: "Termination protection cannot be updated due to stack [" ... "] being a substack"
  variable part: {nested_stack_id} — the nested stack's id/ARN exactly as passed in StackName

Actual (emulator):
  HTTP 400
  Error.Code    = ValidationError  (matches)
  Error.Message = "Cannot change termination protection directly on the nested stack [arn:aws:cloudformation:<region>:<account>:stack/<parent>-ChildNestedStackChildNestedStackResource<suffix>/<uuid>]. For nested stacks, termination protection is set on the root stack [arn:aws:cloudformation:<region>:<account>:stack/<parent>/<uuid>]."

Replay failure:
  File "check_cloudformation_cdk_nested_stack_is_tracked_and_cascades_on_delete.py", line 120, in run
    assert protect.response["Error"]["Message"] == (
  AssertionError: Cannot change termination protection directly on the nested stack [arn:aws:cloudformation:us-west-2:000000000000:stack/sim2real-cdknest-fa425c24be2d-ChildNestedStackChildNestedStackResourceC40294CA-F449D90F5E14/1ac27b64-af32-4a33-65ed-4796f34146e5]. For nested stacks, termination protection is set on the root stack [arn:aws:cloudformation:us-west-2:000000000000:stack/sim2real-cdknest-fa425c24be2d/835a3cc4-6c25-6862-39cf-b9f7b9366523].

(The ARNs, account number, generated stack names and UUIDs above are this run's values and are not part of the requirement; the fixed sentence and the single interpolated stack id are.)

Smithy (services/service-com-amazonaws-cloudformation-cloudformation): UpdateTerminationProtection input {StackName: StackNameOrId required, EnableTerminationProtection: boolean required}, output {StackId}, errors [] — no modelled error, so ValidationError is the protocol's common error and its message is recorded behavior only.

## Steps to Reproduce
1. Deploy a CloudFormation stack that contains a nested stack (the check uses `cdk deploy` of a parent stack with an AWS::CloudFormation::Stack child; any parent template with a nested stack reproduces it). Wait for CREATE_COMPLETE.
2. Read the child's StackId from DescribeStacks / DescribeStackResources on the parent (the nested stack's ParentId/RootId point at the parent).
3. Call cloudformation:UpdateTerminationProtection(StackName=<nested stack id>, EnableTerminationProtection=true).
4. Observe the refusal: HTTP 400, Error.Code=ValidationError (correct), but Error.Message is "Cannot change termination protection directly on the nested stack [<nested arn>]. For nested stacks, termination protection is set on the root stack [<root arn>]." instead of AWS's "Termination protection cannot be updated due to stack [<nested stack id>] being a substack".
5. Control (passes today): the same call against the parent/root stack is accepted and DescribeStacks reports EnableTerminationProtection=true.

Replay to confirm the fix: cloudformation.cdk-nested-stack-is-tracked-and-cascades-on-delete
