Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.guardrail-update-to-nonexistent-kms-key-rejected-key-unchanged
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock.guardrail-update-to-nonexistent-kms-key-rejected-key-unchanged|_error#6|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: c917baaf10025351922dbd2d0dcb1fd84ad836e2

# bedrock CreateGuardrail/UpdateGuardrail accept a well-formed but nonexistent kmsKeyId ARN instead of refusing 400 ValidationException "The kmsKeyId provided is invalid."

> _Note: this issue was identified against the code base at commit `c917baaf10025351922dbd2d0dcb1fd84ad836e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateGuardrail

## Description
One net-new root cause in the bedrock control plane: the guardrail write path never resolves the customer-supplied kmsKeyId against KMS, so a syntactically valid ARN naming a key that does not exist is accepted and a guardrail is created (or updated) encrypted with nothing.

=== Cause 1 of 1: kmsKeyId is pattern-checked but never resolved, so a nonexistent key ARN is accepted ===

AWS: Smithy (com.amazonaws.bedrock) CreateGuardrail = POST /guardrails, success code 202, required members name/blockedInputMessaging/blockedOutputsMessaging, optional kmsKeyId -> KmsKeyId (@length 1..2048, @pattern "^(arn:aws(-[^:]+)?:kms:[a-zA-Z0-9-]*:[0-9]{12}:((key/[a-zA-Z0-9-]{36})|(alias/[a-zA-Z0-9-_/]+)))|([a-zA-Z0-9-]{36})|(alias/[a-zA-Z0-9-_/]+)$"); modelled errors include ValidationException (httpError 400) and ResourceNotFoundException (404). UpdateGuardrail = PUT /guardrails/{guardrailIdentifier}, success 202, carries the same kmsKeyId member and the same ValidationException (400). The @pattern constrains only the SHAPE of the identifier; existence of the key is a service-side pre-condition the model cannot express. Message: recorded behavior only — a kmsKeyId that matches the pattern but names no key in the account/region is refused with HTTP 400, Error.Code "ValidationException", Error.Message "The kmsKeyId provided is invalid.". That message is wholly FIXED text: it has no variable parts, and in particular it does not interpolate the submitted key id, the guardrail name or the region. The refusal is also effect-free — no guardrail id is minted and the rejected name does not appear in ListGuardrails; on UpdateGuardrail the stored kmsKeyArn and updatedAt are left untouched.

Emulator: the request is ACCEPTED. CreateGuardrail with kmsKeyId=arn:aws:kms:<region>:<account>:key/<random uuid> (a key that was never created) answers HTTP 202 with a full success body — guardrailId, guardrailArn, version "DRAFT" and createdAt — so a guardrail is persisted referencing a nonexistent CMK. The key is never looked up in the kms state at all: only the Smithy pattern is applied. The check fails at its first assertion, so the UpdateGuardrail half of the same gap (update to a nonexistent key ARN, which must be refused with the identical ValidationException and must not move kmsKeyArn or updatedAt) is never reached in this replay — but it is the same missing resolution step in the same write path and must be fixed on both operations together.

Why: a client that configures customer-managed-key encryption for a guardrail gets a 202 and a guardrail ARN, and therefore believes its guardrail config is encrypted under its own CMK. In production that request is rejected at create time, which is the only moment the caller can correct a mistyped or deleted key. Against the emulator the bad key id is silently baked into a live resource, the caller's error branch for an invalid key is unreachable, and the effect-absent guarantee AWS gives (nothing created on refusal; the previously stored key preserved on a refused update) is inverted into a created/overwritten resource. Anything downstream that asserts "guardrail X is encrypted with key Y" diverges.

Fix: CreateGuardrail's and UpdateGuardrail's validation must resolve kmsKeyId against KMS key state (ARN, bare key id and alias forms) after the pattern check and before any mutation, and refuse unresolvable keys with the modelled ValidationException, leaving guardrail state and updatedAt unchanged. The handlers are in the bedrock control-plane crate (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice) alongside the other guardrail write ops.

Related but NOT covering this: open issue 20261029T172312Z-bedrock-control-plane-is-over-permissive-enforced-guardrail.md bundles other over-permissive guardrail/inference-profile pre-conditions, and its Cause 5 is CreateGuardrail's uncompilable sensitiveInformationPolicyConfig regex — a different input member and a different validation step. kmsKeyId existence is not among its seven causes.

Traces: bedrock.guardrail-update-to-nonexistent-kms-key-rejected-key-unchanged.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, service bedrock (161/192 passed).

[mismatch] bedrock.guardrail-update-to-nonexistent-kms-key-rejected-key-unchanged
  request: CreateGuardrail(name=<unique>, blockedInputMessaging="Input blocked by guardrail.", blockedOutputsMessaging="Output blocked by guardrail.", wordPolicyConfig={"wordsConfig":[{"text":"<marker>"}]}, kmsKeyId="arn:aws:kms:<region>:<account>:key/<fresh uuid4, never created>")
  expected: HTTP 400, Error.Code "ValidationException", Error.Message "The kmsKeyId provided is invalid." (message is entirely fixed text; no request/state values interpolated), and no guardrail created — the name must not appear in ListGuardrails.
  actual:   HTTP 202 success body: {'guardrailId': <id>, 'guardrailArn': 'arn:aws:bedrock:<region>:<account>:guardrail/<id>', 'version': 'DRAFT', 'createdAt': <timestamp>}
  harness assertion: check_bedrock_guardrail_update_to_nonexistent_kms_key_rejected_key_unchanged.py line 69 -> _error() line 25: AssertionError: expected the call to be rejected, but it succeeded.

The same check's later, unreached assertions document the update-side half of the gap:
  UpdateGuardrail(guardrailIdentifier=<id>, kmsKeyId=<nonexistent key ARN>, ...) must be refused with the same ('ValidationException', 'The kmsKeyId provided is invalid.') pair, and GetGuardrail afterwards must still report the originally stored kmsKeyArn with updatedAt unmoved.

Model facts used (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json):
  CreateGuardrail POST /guardrails code 202; kmsKeyId -> KmsKeyId @length 1..2048 @pattern ^(arn:aws(-[^:]+)?:kms:...key/[a-zA-Z0-9-]{36}|alias/...)$; errors ValidationException 400, ResourceNotFoundException 404, ConflictException 400, AccessDeniedException 403, ServiceQuotaExceededException 400, TooManyTagsException 400, ThrottlingException 429, InternalServerException 500.
  UpdateGuardrail PUT /guardrails/{guardrailIdentifier} code 202, idempotent; same kmsKeyId member; errors ValidationException 400, ResourceNotFoundException 404, ConflictException 400, AccessDeniedException 403, ServiceQuotaExceededException 400, ThrottlingException 429, InternalServerException 500.
  Note: the pattern validates only identifier syntax — key existence is a service-side pre-condition, hence "recorded behavior only" for the message text.

## Steps to Reproduce
Boot the emulator from this checkout and replay bedrock.guardrail-update-to-nonexistent-kms-key-rejected-key-unchanged (suite simparity-behavioral-aws-v1). Manually:
1. Build a well-formed but nonexistent key ARN: arn:aws:kms:<region>:<account>:key/<a fresh uuid4>. Do not create that key.
2. CreateGuardrail(name=<unique>, blockedInputMessaging="Input blocked by guardrail.", blockedOutputsMessaging="Output blocked by guardrail.", kmsKeyId=<that ARN>, wordPolicyConfig={"wordsConfig":[{"text":"<any word>"}]}).
   Expect HTTP 400 ValidationException "The kmsKeyId provided is invalid."; observe HTTP 202 with guardrailId/guardrailArn/version DRAFT/createdAt.
3. ListGuardrails and confirm the name is absent. Expect absent; observe the guardrail listed.
4. Positive control: kms.CreateKey, then issue the identical CreateGuardrail with that real key's ARN. Expect 202 and GetGuardrail.kmsKeyArn == that ARN.
5. UpdateGuardrail(guardrailIdentifier=<the guardrail from step 4>, name=<same>, blockedInputMessaging/blockedOutputsMessaging=<same>, kmsKeyId=<the nonexistent ARN from step 1>, wordPolicyConfig=<same>).
   Expect HTTP 400 ValidationException "The kmsKeyId provided is invalid.", with GetGuardrail still reporting the real key's ARN and updatedAt unchanged; observe the update accepted.
