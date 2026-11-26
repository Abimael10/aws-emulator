Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-automated-reasoning-policy-lifecycle
Issue-Step: teardown#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-automated-reasoning-policy-lifecycle
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|DeleteAutomatedReasoningPolicy|2xx->4xx|status
Issue-Commit: 8158354380cf2730bfcf967251c7e659abcc621b

# bedrock DeleteAutomatedReasoningPolicy answers 404 for the second teardown delete AWS accepts with 202

> _Note: this issue was identified against the code base at commit `8158354380cf2730bfcf967251c7e659abcc621b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAutomatedReasoningPolicy

## Description
One net-new root cause in the bedrock control plane's Automated Reasoning policy delete path, found by replaying the wire corpus simparity-wire-aws-v1 against the emulator built from this checkout. There is 1 cause to close.

=== Cause 1 of 1: the delete AWS accepts (HTTP 202) is answered 404 ResourceNotFoundException, because the preceding unforced delete already removed the policy ===

AWS: Smithy (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json) models DeleteAutomatedReasoningPolicy as DELETE /automated-reasoning-policies/{policyArn} with success code 202 and an EMPTY output shape, @idempotent, input policyArn (@httpLabel, @required, @length 1..2048, @pattern ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:automated-reasoning-policy/[a-z0-9]{12}(:([1-9][0-9]{0,11}))?$ — so the label accepts either a bare policy ARN or a version-qualified one) and force (@httpQuery force, @default false). Its error set is AccessDeniedException 403, ConflictException 400, ResourceInUseException 400, ValidationException 400, ResourceNotFoundException 404, ThrottlingException 429, InternalServerException 500. In this recorded teardown AWS answered the FIRST delete of the lifecycle's policy with a 400-class refusal (teardown#9) and then answered the NEXT delete, the one this step records, with HTTP 202 and the empty success body: the policy was still present and deletable at that point, and this delete is the one that actually removes it. The fixture asserts the status only, so no body member is a requirement here; the specific error code/message AWS would use on a dead ARN is recorded behavior only and is not asserted by this step.

Emulator: this step is answered HTTP 404 (the modelled ResourceNotFoundException class) instead of 202. Replay detail for the cluster: "status: expected 202, got 404"; divergence_key service=bedrock|DeleteAutomatedReasoningPolicy|status=202->404|keys=status. No response body member is asserted or reported as diverging — the whole divergence is the status class: a delete AWS performs is reported as a delete of something that does not exist. The cause is state, not the 404 serializer: the emulator's handler accepts EVERY delete unconditionally (the preceding step in the same fixture, teardown#9, answered 202 where AWS refused 400 — owned by open issue 20261117T165421Z-bedrock-resource-refusal-paths-diverge-deleteautomatedreasoningpolicy-accepts-a.md), so by the time the accepted delete arrives the policy has already been purged from state and the ARN resolves to nothing. Equivalently: the emulator has exactly one delete outcome where AWS has two (refuse-then-delete), so the two recorded deletes land on the wrong side each time.

Why: a client that follows AWS's teardown contract — attempt the unforced delete, then delete again once the blocking dependents are cleared or with force set — gets a success it must not get on the first call and a not-found on the second, so the call that is supposed to be its confirmation that the policy is gone instead reports that the policy never existed. Reconciliation and teardown code that treats 404 as "someone else removed this" or as a fatal inconsistency diverges, and because the modelled 202 empty-body success is unreachable on the real delete, the success branch of every such caller is never exercised against the emulator. Retry logic built on the operation's @idempotent contract is also broken: repeating the delete flips from success to not-found rather than staying stable.

Fix: DeleteAutomatedReasoningPolicy's handler, together with the Automated Reasoning policy state layer it mutates, must evaluate the delete's preconditions BEFORE mutating state and only remove the policy on the delete it actually performs — i.e. the unforced delete of a policy with dependents must leave state untouched (refusing with the modelled 400 client error, which is the cause the issue above owns), so that the subsequent delete still resolves its policyArn and answers the modelled 202 with the empty body. The resolution must honour both forms the @pattern allows (bare and version-qualified policy ARN) and must distinguish "policy present, delete performed" (202) from "ARN resolves to nothing" (404) rather than deriving the answer from a single unconditional purge.

Traces: bedrock-automated-reasoning-policy-lifecycle#teardown#10 (expected 202, got 404). Related context in the same fixture, NOT covering this step: teardown#9 (expected 400, got 202) is owned by 20261117T165421Z-bedrock-resource-refusal-paths-diverge-deleteautomatedreasoningpolicy-accepts-a.md, whose replay key is teardown#9 and whose requirement is the refusal — it does not carry this step and does not state the 202 requirement; task#2/#4/#6 (definitionHash and version creation) are owned by 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md; and 20261115T054942Z-bedrock-deleteautomatedreasoningpolicy-accepts-a-deleted-policy-arn-with.md is a behavioral-corpus issue about a second delete of an ALREADY-DELETED ARN (expecting 404), i.e. the opposite expectation on a dead ARN, with no wire replay key.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: emulator_up=true, bootstrap_signal=false, 33 fixtures replayed, 189 steps (155 passed, 34 mismatched, 0 errored, 0 blocked), score 82.0%, 19 divergence clusters. 18 of the 19 are already owned by an open issue's replay key; the cluster below is the only unowned one and it is service=bedrock.

[mismatch] bedrock / DeleteAutomatedReasoningPolicy — fixture bedrock-automated-reasoning-policy-lifecycle, step teardown#10
  divergence_key: service=bedrock|DeleteAutomatedReasoningPolicy|status=202->404|keys=status
  expected: HTTP 202 with the modelled empty success body (the delete is performed)
  actual:   HTTP 404 (modelled ResourceNotFoundException class) — the policy ARN resolves to nothing
  detail:   "status: expected 202, got 404"
  affected: bedrock-automated-reasoning-policy-lifecycle#teardown#10 (1 step)
  owned_by: [] (no open issue carries this replay key)
  context in the same fixture: teardown#9 DeleteAutomatedReasoningPolicy diverges the other way (expected 400, got 202) and IS owned (20261117T165421Z-...); the emulator's unconditional accept at #9 is what leaves no policy for #10 to delete.

Model facts used (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json): DeleteAutomatedReasoningPolicy — DELETE /automated-reasoning-policies/{policyArn}, http code 202, @idempotent, empty output shape; input policyArn @httpLabel @required @length 1..2048 @pattern ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:automated-reasoning-policy/[a-z0-9]{12}(:([1-9][0-9]{0,11}))?$, force @httpQuery force @default false; errors AccessDeniedException 403 / ConflictException 400 / ResourceInUseException 400 / ValidationException 400 / ResourceNotFoundException 404 / ThrottlingException 429 / InternalServerException 500. CreateAutomatedReasoningPolicyVersion — POST /automated-reasoning-policies/{policyArn}/versions, http code 200, input lastUpdatedDefinitionHash @required (@length 128..128, @pattern ^[0-9a-z]{128}$), output {policyArn, version, name, description, definitionHash, createdAt}.

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1; the cluster is fixture bedrock-automated-reasoning-policy-lifecycle, step teardown#10. By hand against the emulator's bedrock control-plane endpoint:

1. CreateAutomatedReasoningPolicy(name=<unique>, policyDefinition=<definition>) -> note policyArn and definitionHash.
2. UpdateAutomatedReasoningPolicy(policyArn, <changed definition>), then CreateAutomatedReasoningPolicyVersion(policyArn, lastUpdatedDefinitionHash=<hash from the update>) so the policy has a version/dependent (against the emulator this step currently 400s; that part is owned by the definitionHash issue).
3. DeleteAutomatedReasoningPolicy(policyArn) with force unset (default false) -> AWS refuses with a 400-class client error; the emulator answers 202 (owned by 20261117T165421Z-...).
4. Delete the policy again as the recorded teardown does (the delete AWS performs) -> expect HTTP 202 with an empty body; observe HTTP 404, because step 3 already purged the policy from emulator state so the policyArn no longer resolves.

The fix is confirmed when step 4 answers 202 with the empty body (and step 3 leaves the policy in state).
