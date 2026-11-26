Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: qconnect.ai-prompt-version-concurrent-creation-race
Issue-Shape-Id: com.amazonaws.qconnect#WisdomService
Issue-Key: qconnect.ai-prompt-version-concurrent-creation-race|_poll_until#8|AssertionError|raise AssertionError(f"timed out after {timeout_s}s waiting for {description}, last value={value!r}")
Issue-Commit: c00bf07ce89c73b0369514a40ab33e3142015054

# qconnect CreateAIPromptVersion answers 501 NotImplemented, so no AI Prompt version can ever be published

> _Note: this issue was identified against the code base at commit `c00bf07ce89c73b0369514a40ab33e3142015054`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAIPromptVersion

## Description
One net-new root cause in the qconnect AI-Prompt version plane.

Cause 1 of 1: `CreateAIPromptVersion` is an unimplemented scaffold stub, so publishing a version of an AI Prompt is impossible and every concurrency/ordering property of that operation is unobservable. Failing check: qconnect.ai-prompt-version-concurrent-creation-race.

AWS: Smithy (services/service-com-amazonaws-qconnect-wisdomservice/smithy-model/com-amazonaws-qconnect-wisdomservice.json) models `CreateAIPromptVersion` as POST /assistants/{assistantId}/aiprompts/{aiPromptId}/versions, @idempotent, success HTTP 200, output `{aiPrompt: AIPromptData, versionNumber: Version (long, range min 1)}`. Its only modeled failures are AccessDeniedException (403), ConflictException (409), ResourceNotFoundException (404), ServiceQuotaExceededException (402), ThrottlingException (400), UnauthorizedException (401), ValidationException (400) — there is no NotImplemented in the closure. For a prompt whose `visibilityStatus` has been moved to PUBLISHED by UpdateAIPrompt, AWS accepts the call and returns 200 with a fresh, monotonically increasing `versionNumber` that is unique across concurrent callers; the recorded behavior of concurrent racers is that at most one racer per version number succeeds and a losing racer is refused 400 `ValidationException` with the message `Failed to update SAVED alias.` (fixed text, no variable parts), and `ListAIPromptVersions` then reports every version number that was returned 200.

Emulator: every CreateAIPromptVersion request is refused by the scaffold stub in services/service-com-amazonaws-qconnect-wisdomservice/src/ops/create_ai_prompt_version.rs — `handle` returns `Err(crate::scaffolded::Scaffolded::not_implemented("CreateAIPromptVersion"))`, i.e. HTTP 501 with error code `NotImplemented`, for a well-formed request on an ACTIVE assistant holding a PUBLISHED prompt. The check polls the call for 30 s and never gets a success: `AssertionError: timed out after 30.0s waiting for CreateAIPromptVersion to succeed after publishing the AI Prompt, last value=None`. Because the refusal is permanent, nothing downstream in the version plane (the 5-way race, the duplicate-version-number assertion, the `Failed to update SAVED alias.` refusal of a losing racer, and the ListAIPromptVersions read-back) is exercised at all.

Why: a client that publishes prompt versions — the normal way a Q in Connect AI Prompt is promoted for use, and what AWS::Wisdom::AIPromptVersion deploys — gets 501 `NotImplemented` instead of 200 with a `versionNumber`. 501 is not a modeled outcome of this operation, so SDK/Botocore error handling cannot classify it: it is neither retryable throttling nor a client-side validation failure, and no amount of retrying or input correction makes it succeed. Version-pinned reads (GetAIPrompt on `<id>:<version>`), ListAIPromptVersions and any workflow that activates a published prompt are unreachable.

Fix: implement the operation's handler, services/service-com-amazonaws-qconnect-wisdomservice/src/ops/create_ai_prompt_version.rs, against the shared AI-Prompt state (crate::state): resolve assistant then prompt (404 ResourceNotFoundException otherwise), honour the @idempotencyToken `clientToken` the way update_ai_prompt.rs already does, snapshot the prompt's working copy under the next version number, and serialize `aiPrompt` + `versionNumber`; make the version-number allocation and the SAVED-alias move atomic under the state lock so concurrent callers cannot be handed the same number, refusing the loser 400 ValidationException with the message `Failed to update SAVED alias.`, and make the new version visible to ListAIPromptVersions. Delete the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker once done.

Traces: qconnect.ai-prompt-version-concurrent-creation-race (mismatch). Note: the open issue 20261113T130046Z-qconnect-updateaiprompt-stub.md covers UpdateAIPrompt (which is now implemented in-tree) and does not cover CreateAIPromptVersion; no open issue names this operation. This failure was NOT marked ALREADY FILED by the suite because the check swallows the ClientError from the 501 and fails on its own poll timeout, so the stub-detection never saw the 501 propagate.

## Evidence
Check qconnect.ai-prompt-version-concurrent-creation-race.

Setup that succeeded: CreateAssistant -> ACTIVE; CreateAIPrompt(type=QUERY_REFORMULATION, templateType=TEXT, apiFormat=MESSAGES, visibilityStatus=SAVED); UpdateAIPrompt(visibilityStatus=PUBLISHED) -> 200.

Expected (AWS): CreateAIPromptVersion(assistantId, aiPromptId) -> HTTP 200 with {aiPrompt: {...}, versionNumber: <N >= 1>}; 5 concurrent racers get distinct versionNumbers, any loser is HTTP 400 ValidationException with Message exactly "Failed to update SAVED alias."; ListAIPromptVersions reports every successfully returned versionNumber.

Actual (emulator): every CreateAIPromptVersion call is refused HTTP 501, error code NotImplemented (scaffold stub), for 30 s of polling. Replay output:
  File "check_qconnect_ai_prompt_version_concurrent_creation_race.py", line 58, in run
    first_version = _poll_until(
  AssertionError: timed out after 30.0s waiting for CreateAIPromptVersion to succeed after publishing the AI Prompt, last value=None

Source: services/service-com-amazonaws-qconnect-wisdomservice/src/ops/create_ai_prompt_version.rs line 4 (`//! SCAFFOLDED, NOT IMPLEMENTED:`) and line 180 (`Err(crate::scaffolded::Scaffolded::not_implemented("CreateAIPromptVersion"))`). 501/NotImplemented is not among the operation's modeled errors (403/409/404/402/400/401/400).

## Steps to Reproduce
1. qconnect CreateAssistant(name=<unique>, type=AGENT); poll GetAssistant until status == ACTIVE.
2. CreateAIPrompt(assistantId, name=<unique>, type=QUERY_REFORMULATION, templateType=TEXT, apiFormat=MESSAGES, modelId=us.amazon.nova-lite-v1:0, visibilityStatus=SAVED, templateConfiguration.textFullAIPromptEditTemplateConfiguration.text=<any valid prompt text>).
3. UpdateAIPrompt(assistantId, aiPromptId, visibilityStatus=PUBLISHED) -> 200.
4. CreateAIPromptVersion(assistantId, aiPromptId). Observe HTTP 501 / NotImplemented instead of 200 with a versionNumber; repeating the call never succeeds.
5. (Blocked by step 4) Fire 5 concurrent CreateAIPromptVersion calls and assert the returned versionNumbers are distinct, that any failure is 400 ValidationException "Failed to update SAVED alias.", and that ListAIPromptVersions(assistantId, aiPromptId) lists every returned versionNumber.
