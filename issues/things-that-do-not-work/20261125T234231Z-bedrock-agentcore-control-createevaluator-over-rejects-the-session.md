Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.update-deleted-evaluator-rejected
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|CreateEvaluator|ValidationException
Issue-Commit: b3f2da0c21167fbe74db2402ccf200ff7e080a5b

# bedrock-agentcore-control CreateEvaluator over-rejects: the SESSION placeholder set is applied to TRACE, so a TRACE evaluator AWS accepts is refused 400 ValidationException

> _Note: this issue was identified against the code base at commit `b3f2da0c21167fbe74db2402ccf200ff7e080a5b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEvaluator

## Description
REOPENS 20261123T075059Z-bedrock-agentcore-control-evaluator-create-get-diverge-missing.md: its fix left the call refused. That issue's Cause 1 asked for the per-level allowed-placeholder check on EvaluatorConfig.llmAsAJudge.instructions, grounded in a SESSION-level refusal. The implemented check installs that one published set for EVERY level (services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state/evaluators.rs lines 163-189: INSTRUCTION_PLACEHOLDERS mapped identically to TOOL_CALL, TRACE and SESSION in INSTRUCTION_PLACEHOLDERS_BY_LEVEL), so it now refuses a TRACE-level create AWS accepts.

Cause 1 of 1: CreateEvaluator at level=TRACE is refused when the llmAsAJudge instructions reference a per-turn placeholder instead of one of the SESSION-level names.
  AWS: Smithy CreateEvaluator — POST /evaluators/create, success code 202; input evaluatorConfig (union EvaluatorConfig, required), level (enum EvaluatorLevel, required: TOOL_CALL|TRACE|SESSION), evaluatorName (pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}$); modeled ValidationException is errorKind client, httpError 400. The model states NO placeholder rule for LlmAsAJudgeEvaluatorConfig.instructions (smithy_query doc shape=LlmAsAJudgeEvaluatorConfig member=instructions documents only "The evaluation instructions that guide the language model in assessing agent performance, including criteria and evaluation guidelines."), so the allowed set per level is recorded behavior only. The recording shows AWS ACCEPTING, with HTTP 202 and a created evaluator, a level=TRACE create whose llmAsAJudge.instructions are "Rate the helpfulness of the assistant's response on a 1-5 scale. Assistant response: {assistant_turn}" — i.e. instructions that reference NONE of available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions. The only refusal this family has on record is the SESSION one already quoted in the reopened issue; nothing recorded says those five names are the TRACE set. Hypothesis: TRACE evaluates a single request-response interaction, so it additionally allows per-turn placeholders such as assistant_turn.
  Emulator: refuses the TRACE create with HTTP 400 ValidationException, message "Instructions must contain at least one of the allowed placeholders for TRACE level: available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions" (fixed text "Instructions must contain at least one of the allowed placeholders for " + <level, from the request's level member> + " level: " + <comma-separated allowed names for that level>; the level token TRACE and the five names are the variable parts, and the five names are the value that is wrong here), repeated verbatim in a one-entry fieldList naming member "instructions", with reason=fieldValidationFailed.
  Why: a client creating a TRACE-level LLM-as-a-Judge evaluator with a per-turn prompt — the normal TRACE shape — can never create one at all. The refusal happens before any state is written, so every downstream flow (UpdateEvaluator, DeleteEvaluator, polling GetEvaluator to ACTIVE, and any online-evaluation configuration that references the evaluator) is unreachable against this emulator, and a client's rejection path is exercised where AWS returns 202. The over-rejection also silently hides whether the under-validation that the reopened issue fixed was fixed correctly, because no TRACE prompt can reach the store.
  Fix: the shared evaluator-instructions validation used by CreateEvaluator and UpdateEvaluator (crate::state::validate_evaluator_instructions and the per-level table it reads, in the service's state/evaluators.rs; called from ops/create_evaluator.rs). The per-level table must not reuse the SESSION-derived set for TRACE: TRACE must accept instructions that reference only a per-turn placeholder such as assistant_turn, and the refusal message must list the set actually allowed for the level it names. Keep the check shared between create and update so one body is not accepted by one and refused by the other.
  Traces: bedrock-agentcore-control.update-deleted-evaluator-rejected (the check never reaches its UpdateEvaluator assertion — it dies on the first CreateEvaluator).

## Evidence
Behavioral suite simparity-behavioral-aws-v1, 118/148 passed. Failing check bedrock-agentcore-control.update-deleted-evaluator-rejected [error], not marked ALREADY FILED and not OWNED by any open issue:

  botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the CreateEvaluator operation: Instructions must contain at least one of the allowed placeholders for TRACE level: available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions

The request the check makes (and that the recording shows AWS accepting with 202):
  create_evaluator(evaluatorName=<alnum name>, level="TRACE", tags={...}, evaluatorConfig={"llmAsAJudge": {"instructions": "Rate the helpfulness of the assistant's response on a 1-5 scale. Assistant response: {assistant_turn}", "ratingScale": {"numerical": [{"label": "poor", "value": 1.0, ...}, {"label": "excellent", "value": 5.0, ...}]}, "modelConfig": {"bedrockEvaluatorModelConfig": {"modelId": "amazon.nova-lite-v1:0"}}}})

Expected: HTTP 202 with evaluatorId/evaluatorArn/createdAt/status, after which UpdateEvaluator on a deleted evaluator is refused with ResourceNotFoundException "Evaluator does not exist" and an update of a live one settles ACTIVE with the new description.
Actual: HTTP 400 ValidationException on the very first CreateEvaluator; no evaluator is created and none of the check's assertions are reached.

Emulator source carrying the defect: services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state/evaluators.rs:163-189 — INSTRUCTION_PLACEHOLDERS (five SESSION-published names) is mapped to TOOL_CALL, TRACE and SESSION alike, and allowed_instruction_placeholders() falls back to that same set for any other level; validate_evaluator_instructions() (same file, ~line 202) accepts only instructions containing one of them, and is invoked from src/ops/create_evaluator.rs:192 before any state is touched.

Model grounding: smithy_query operation=CreateEvaluator -> POST /evaluators/create, code 202, errors include ValidationException (client, httpError 400); level is a required EvaluatorLevel enum (TOOL_CALL|TRACE|SESSION); the model documents no placeholder constraint on instructions.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the behavioral suite simparity-behavioral-aws-v1.
3. Run check bedrock-agentcore-control.update-deleted-evaluator-rejected, or directly call CreateEvaluator (POST /evaluators/create) with level="TRACE" and evaluatorConfig={"llmAsAJudge": {"instructions": "Rate the helpfulness of the assistant's response on a 1-5 scale. Assistant response: {assistant_turn}", "ratingScale": {"numerical": [{"label": "poor", "value": 1.0, "definition": "Not helpful at all"}, {"label": "excellent", "value": 5.0, "definition": "Extremely helpful"}]}, "modelConfig": {"bedrockEvaluatorModelConfig": {"modelId": "amazon.nova-lite-v1:0"}}}}.
4. Expect HTTP 202 with a created evaluator (status settling ACTIVE). Observe HTTP 400 ValidationException "Instructions must contain at least one of the allowed placeholders for TRACE level: available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions" with a one-entry fieldList naming 'instructions'.
5. Confirm the SESSION refusal still holds after the fix: the same request with level="SESSION" and instructions referencing none of the SESSION placeholders must still be refused 400 with the SESSION wording (the behavior the reopened issue installed).
