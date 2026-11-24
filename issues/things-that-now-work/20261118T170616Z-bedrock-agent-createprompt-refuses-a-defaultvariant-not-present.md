Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent.create-prompt-variant-mismatch-validation-error
Issue-Shape-Id: com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda
Issue-Key: bedrock-agent.create-prompt-variant-mismatch-validation-error|run#6|AssertionError|assert message == (
Issue-Commit: 0a24b18ae59262f0cdae7b98999a169258844aef

# bedrock-agent CreatePrompt refuses a defaultVariant not present in variants with the generic constraint-envelope message instead of AWS's cross-member sentence

> _Note: this issue was identified against the code base at commit `0a24b18ae59262f0cdae7b98999a169258844aef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePrompt

## Description
ONE net-new root cause, reached through the bedrock-agent behavioral check the replay report lists under `### bedrock-agent` and left unmarked (neither ALREADY FILED nor OWNED).

=== Cause 1 of 1: CreatePrompt's defaultVariant-membership refusal carries the wrong message — the emulator renders the cross-member rule as a per-member @pattern/constraint envelope, AWS emits a fixed cross-member sentence ===
AWS: Smithy (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/smithy-model/com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda.json): CreatePrompt input has `defaultVariant` (PromptVariantName, @pattern ^([0-9a-zA-Z][_-]?){1,100}$, OPTIONAL) and `variants` (PromptVariantList, @length max 1, OPTIONAL), and the operation models ValidationException (httpError 400). The model expresses no cross-member rule between the two members, so the recording supplies it. Recorded behavior: a CreatePrompt whose `defaultVariant` names no entry of the supplied `variants` is refused ValidationException (HTTP 400) with the message `Default variant must be present in the variants list. Retry your request with the correct default variant.` That message is entirely FIXED text — AWS interpolates NOTHING from the request into it: neither the offending defaultVariant value, nor the member name, nor the names of the supplied variants. The rejected prompt is not created (it does not appear in ListPrompts), and the same call with `defaultVariant` set to a supplied variant's name succeeds and reads back with that defaultVariant.
Emulator: the refusal now arrives with the correct code (ValidationException), so the pre-condition itself is being evaluated, but the message is the emulator's own per-member constraint envelope: `Value 'a_name_not_in_variants' at 'defaultVariant' failed to satisfy constraint: Member must match the name of one of the variants provided for the prompt`. Here `Value '`, `' at '`, `' failed to satisfy constraint: Member must …` is fixed emulator text and `a_name_not_in_variants` / `defaultVariant` are interpolated from the request — i.e. the emulator formats a cross-member rule through the single-member @pattern/constraint template, and AWS's sentence (including its `Retry your request with the correct default variant.` remediation clause) is absent. This is a pure wording divergence on the message; the status and error code match.
Why: ValidationException is a single generic code shared by every input defect of the operation, so the message text is the only machine-usable discriminator a client has. A caller or deployment tool that branches on AWS's sentence (to re-send with a corrected default variant, or to surface "your default variant is not one of the variants you declared" to an operator) does not match the emulator's text and falls through to its unknown-error path; a tool that echoes the message verbatim shows a constraint-envelope sentence the service never emits, and never shows AWS's remediation clause.
Fix: the CreatePrompt handler's validation in the bedrock-agent crate (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/create_prompt.rs) — emit AWS's fixed cross-member sentence for the defaultVariant-membership pre-condition rather than routing it through the shared per-member constraint-message formatter, keeping the refusal before any state is touched so no prompt is left behind.
NOTE on related open work: open issue 20261030T000542Z-bedrock-agent-5-net-new-causes-vector-knowledge.md describes this same pre-condition as its Cause 3, but it was filed against the earlier behavior where the call fell through to the unimplemented scaffold and answered `NotImplemented`; its replay key for this check is the code assertion (`assert code == "ValidationException"`), which now PASSES, and the replay report does not mark this check OWNED. The fix for the scaffold itself landed (fixed issue 20261030T000542Z-bedrock-agent-createprompt-stub.md), so what remains and is recorded here is the message wording, which no issue carries as its replay key.
Traces: bedrock-agent.create-prompt-variant-mismatch-validation-error.

## Evidence
Check bedrock-agent.create-prompt-variant-mismatch-validation-error, [mismatch], AssertionError at check_bedrock_agent_create_prompt_variant_mismatch_validation_error.py line 30 (`assert message == (...)`).

Request: CreatePrompt(name=<valid generated prompt name>, defaultVariant='a_name_not_in_variants', variants=[{'name': 'actual_variant', 'templateType': 'TEXT', 'templateConfiguration': {'text': {'text': 'hello {{name}}'}}}]).

Expected (AWS): error code `ValidationException`, HTTP 400, message exactly `Default variant must be present in the variants list. Retry your request with the correct default variant.` — all fixed text, no request- or state-derived values.

Actual (emulator): error code `ValidationException` (matches), message `Value 'a_name_not_in_variants' at 'defaultVariant' failed to satisfy constraint: Member must match the name of one of the variants provided for the prompt`. Fixed emulator text: `Value '` … `' at '` … `' failed to satisfy constraint: Member must match the name of one of the variants provided for the prompt`; the interpolated slots are the request's defaultVariant value (`a_name_not_in_variants`) and the input member name (`defaultVariant`) — those two are template slots from this run's request, not part of the requirement.

The earlier assertions of the same check pass (code == ValidationException), and the later legs (ListPrompts shows no leaked prompt; the well-formed CreatePrompt + GetPrompt round trip with defaultVariant 'actual_variant') were not reached in this replay because the message assertion aborts first.

## Steps to Reproduce
0. git_clone, then run_behavioral_suite and read the `### bedrock-agent` section of the failing-check list.
1. Call bedrock-agent CreatePrompt with a name matching ^([0-9a-zA-Z][_-]?){1,100}$, defaultVariant='a_name_not_in_variants', and variants=[{'name': 'actual_variant', 'templateType': 'TEXT', 'templateConfiguration': {'text': {'text': 'hello {{name}}'}}}].
2. Observe the ClientError: the code is ValidationException (correct) but the message is the constraint-envelope sentence `Value 'a_name_not_in_variants' at 'defaultVariant' failed to satisfy constraint: Member must match the name of one of the variants provided for the prompt` instead of AWS's `Default variant must be present in the variants list. Retry your request with the correct default variant.`
3. Call ListPrompts and confirm the rejected prompt name did not leak (this part already holds).
4. Re-issue the same CreatePrompt with defaultVariant='actual_variant' and confirm it is accepted and GetPrompt reports defaultVariant='actual_variant'.
