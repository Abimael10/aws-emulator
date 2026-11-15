Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.automated-reasoning-policy-create-delete-404
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock.automated-reasoning-policy-create-delete-404|_error#15|AssertionError|raise AssertionError(f"expected this call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: 3e93d103c367b3f539a01bb50ca5553fc9a2ef76

# bedrock DeleteAutomatedReasoningPolicy accepts a deleted policy ARN with 202 instead of ResourceNotFoundException "Policy was not found"

> _Note: this issue was identified against the code base at commit `3e93d103c367b3f539a01bb50ca5553fc9a2ef76`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAutomatedReasoningPolicy

## Description
One net-new root cause in the bedrock control plane's Automated Reasoning policy state machine.

=== Cause 1 of 1: a second DeleteAutomatedReasoningPolicy on an already-deleted policy ARN is silently accepted ===

AWS: Smithy (com.amazonaws.bedrock, AmazonBedrockControlPlaneService) DeleteAutomatedReasoningPolicy = DELETE /automated-reasoning-policies/{policyArn}, success code 202 with an empty output shape, input policyArn (@httpLabel, @required, @length 1..2048, @pattern ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:automated-reasoning-policy/[a-z0-9]{12}(:([1-9][0-9]{0,11}))?$) plus optional force (@httpQuery force, default false); the operation is marked @idempotent and models ResourceNotFoundException with httpError 404 (alongside AccessDeniedException 403, ConflictException 400, ResourceInUseException 400, ThrottlingException 429, ValidationException 400, InternalServerException 500). Message: recorded behavior only — once the policy has been deleted, the ARN no longer resolves and the service refuses any further call on it. The recording shows two distinct, wholly fixed message texts for the dead ARN, differentiated by operation and NOT interchangeable: GetAutomatedReasoningPolicy -> 404 ResourceNotFoundException with the fixed text "Policy does not exist", and a second DeleteAutomatedReasoningPolicy -> ResourceNotFoundException with the fixed text "Policy was not found". Neither message interpolates anything from the request or from state: both are entirely fixed text, so the policy ARN, name and id must NOT appear in them. @idempotent here describes safe retry semantics on the wire, not a no-op-on-missing contract: AWS still answers ResourceNotFoundException for an ARN that no longer exists.

Emulator: the first delete succeeds and the subsequent GetAutomatedReasoningPolicy correctly refuses 404 ResourceNotFoundException "Policy does not exist" (that assertion passes), but the second DeleteAutomatedReasoningPolicy on the very same, now-deleted policyArn is ACCEPTED: HTTP 202 with an empty JSON body ({}) and no error. The replay fails with `AssertionError: expected this call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {..., 'HTTPStatusCode': 202, ...}}`. So the delete path does not resolve the ARN against policy state before answering — it reports success for a policy that demonstrably does not exist, even though the read path on the same ARN already knows it is gone.

Why: a client cannot distinguish "I deleted it" from "it was never there / someone else deleted it". Teardown and reconciliation logic that branches on ResourceNotFoundException (the common pattern of deleting until 404, or treating 404 as the terminal confirmation of removal) takes the success path forever against the emulator, so double-delete races, idempotent-cleanup helpers and "does this resource still exist" probes that use Delete as the probe all read as present-and-deleted instead of absent. It also hides the error-code/message contract: callers that surface AWS's distinct wording ("Policy was not found" for Delete vs "Policy does not exist" for Get) never see either from this path.

Fix: DeleteAutomatedReasoningPolicy's handler must resolve policyArn against the Automated Reasoning policy state before mutating and, when no policy is registered under that ARN (including one deleted earlier in the same session), answer the modelled ResourceNotFoundException (404) with the fixed message "Policy was not found" instead of the 202 empty success. The read path already resolves the ARN correctly, so the lookup belongs in the delete handler/state layer beside it; the existing distinct Get message must be left as-is, since the two operations' texts differ.

Traces: bedrock.automated-reasoning-policy-create-delete-404.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, service bedrock: 159/191 passed; this is the only failing bedrock check not already owned by an open issue or blocked by an unimplemented scaffold stub.

[mismatch] bedrock.automated-reasoning-policy-create-delete-404
  sequence: CreateAutomatedReasoningPolicy(name=<unique>) -> GetAutomatedReasoningPolicy(policyArn) (OK, name/version round-trip) -> DeleteAutomatedReasoningPolicy(policyArn) (OK) -> GetAutomatedReasoningPolicy(policyArn) -> 404 ResourceNotFoundException "Policy does not exist" (PASSES) -> DeleteAutomatedReasoningPolicy(policyArn) again
  expected: ResourceNotFoundException, Error.Message == "Policy was not found" (wholly fixed text; no ARN/name interpolation)
  actual:   AssertionError: expected this call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'RequestId': '<per-run>', 'HTTPStatusCode': 202, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '2', ...}, 'RetryAttempts': 0}}
  (content-length 2 == the empty JSON object body of the modelled 202 success)

Model facts used (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json): DeleteAutomatedReasoningPolicy DELETE /automated-reasoning-policies/{policyArn}, http code 202, @idempotent, input policyArn @required @httpLabel + force @httpQuery default false, output shape empty, errors ResourceNotFoundException 404 / ConflictException 400 / ResourceInUseException 400 / ValidationException 400 / AccessDeniedException 403 / ThrottlingException 429 / InternalServerException 500. GetAutomatedReasoningPolicy GET same URI, code 200, errors include ResourceNotFoundException 404.

Not covered by the open issue 20261029T172312Z-bedrock-control-plane-is-over-permissive-enforced-guardrail.md: its automated-reasoning cause concerns CreateAutomatedReasoningPolicy inventing a duplicate-name ConflictException, not the delete path's missing existence check. Also not covered by 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md, which concerns definitionHash derivation and version creation.

## Steps to Reproduce
Boot the emulator from this checkout and replay bedrock.automated-reasoning-policy-create-delete-404 (suite simparity-behavioral-aws-v1), or by hand:
1. CreateAutomatedReasoningPolicy(name="sim2real-arpolicy-<unique>") -> note policyArn and version.
2. GetAutomatedReasoningPolicy(policyArn=<arn>) -> succeeds, name and version match the create response.
3. DeleteAutomatedReasoningPolicy(policyArn=<arn>) -> 202.
4. GetAutomatedReasoningPolicy(policyArn=<arn>) -> expect 404 ResourceNotFoundException "Policy does not exist"; the emulator already does this correctly.
5. DeleteAutomatedReasoningPolicy(policyArn=<arn>) a SECOND time -> expect ResourceNotFoundException with message "Policy was not found"; observe HTTP 202 with an empty body and no error.
